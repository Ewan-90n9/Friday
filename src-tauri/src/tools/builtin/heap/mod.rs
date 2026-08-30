pub mod mapping;

use crate::analyzer::manager::{HeapAnalyzerManager, ManagerError};
use crate::tools::builtin::run_command::{artifact_dir_for, truncate_output};
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tools::builtin::jvm::core::{clamp_or, error_output};

/// (default_secs, max_secs)
type Timeouts = (u64, u64);
const OPEN: Timeouts = (600, 1800);
const CLOSE: Timeouts = (30, 60);
const QUERY: Timeouts = (60, 300);

#[derive(Debug, Clone, Copy)]
pub enum HeapToolKind {
    Open,
    Close,
    LeakSuspects,
    Histogram,
    DominatorTree,
    ObjectInfo,
    PathToGcRoots,
    References,
    Threads,
}

pub struct HeapToolHandler {
    pub manager: Arc<HeapAnalyzerManager>,
    pub artifacts_dir: PathBuf,
    pub kind: HeapToolKind,
    pub timeouts: Timeouts,
}

#[async_trait]
impl ToolHandler for HeapToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(local_path) = args.get("local_path").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: local_path");
        };
        let path = match resolve_local_path(local_path) {
            Ok(p) => p,
            Err(e) => return error_output("invalid_params", &e),
        };
        let timeout_secs =
            clamp_or(args.get("timeout_secs").and_then(|v| v.as_i64()), self.timeouts.0, self.timeouts.1);
        let start = std::time::Instant::now();
        tracing::info!(session_id = %ctx.session_id, kind = ?self.kind, dump = %path.display(), "heap tool executing");

        match self.kind {
            HeapToolKind::Open => match self.manager.open(&ctx.session_id, &path, timeout_secs).await {
                Ok(outcome) => {
                    let mut out = render(&ctx.session_id, &self.artifacts_dir, "open_heap_dump", local_path, &outcome.summary, start).await;
                    if !outcome.evicted.is_empty() {
                        out.data["evicted"] = serde_json::json!(
                            outcome.evicted.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
                        );
                    }
                    out
                }
                Err(e) => manager_error_output(e),
            },
            HeapToolKind::Close => match self.manager.close(&path, timeout_secs).await {
                Ok(was_open) => ToolOutput {
                    success: true,
                    data: serde_json::json!({
                        "tool": "close_heap_dump",
                        "local_path": local_path,
                        "was_open": was_open,
                    }),
                    raw_stdout: None,
                },
                Err(e) => manager_error_output(e),
            },
            kind => {
                let (upstream_name, upstream_args) = match mapping::build(kind, &args) {
                    Ok(v) => v,
                    Err(e) => return error_output("invalid_params", &e),
                };
                match self.manager.query(&path, &upstream_name, &upstream_args, timeout_secs).await {
                    Ok(outcome) => {
                        render(&ctx.session_id, &self.artifacts_dir, &upstream_name, local_path, &outcome.text, start).await
                    }
                    Err(e) => manager_error_output(e),
                }
            }
        }
    }
}

/// 结果组装：64KB 头部截断 + 完整结果落盘 session artifacts（复用 run_command 机制）
async fn render(
    session_id: &str,
    artifacts_dir: &Path,
    upstream_tool: &str,
    local_path: &str,
    text: &str,
    start: std::time::Instant,
) -> ToolOutput {
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let (body, truncated) = truncate_output(text);
    let session_dir = artifact_dir_for(artifacts_dir, session_id);
    let artifact_path = session_dir.join(format!("heap-{}.md", uuid::Uuid::new_v4()));
    // 完整输出带自描述头落盘（对齐 run_command 的 full output 持久化格式）
    let full = format!("--- tool: {upstream_tool} ---\n--- local_path: {local_path} ---\n--- full output ---\n{text}\n");
    let mut full_output_path = None;
    if tokio::fs::create_dir_all(&session_dir).await.is_ok() {
        if tokio::fs::write(&artifact_path, &full).await.is_ok() {
            full_output_path = Some(artifact_path);
        } else {
            tracing::warn!(session_id, tool = upstream_tool, "failed to persist full heap tool output");
        }
    }
    let result_field = if truncated {
        match &full_output_path {
            Some(p) => format!("{body}\n[truncated, full output: {}]", p.display()),
            None => format!("{body}\n[truncated]"),
        }
    } else {
        body
    };
    tracing::info!(session_id, tool = upstream_tool, elapsed_ms, truncated, "heap tool executed");
    ToolOutput {
        success: true,
        data: serde_json::json!({
            "tool": upstream_tool,
            "local_path": local_path,
            "result": result_field,
            "elapsed_ms": elapsed_ms,
            "truncated": truncated,
            "full_output_path": full_output_path.as_ref().map(|p| p.display().to_string()),
        }),
        raw_stdout: Some(text.to_string()),
    }
}

/// ManagerError → 结构化错误输出。Upstream（MAT 业务错误）走透传（无 error code，对齐 jvm_* 惯例）。
fn manager_error_output(e: ManagerError) -> ToolOutput {
    match e {
        ManagerError::JavaMissing(m) => {
            error_output("java_missing", &format!("本机 Java 21+ 不可用：{m}。请安装 JDK 21+ 后重试。"))
        }
        ManagerError::Unavailable(m) => error_output(
            "analyzer_unavailable",
            &format!("{m}。可重试一次；连续失败请查看 Friday 日志。"),
        ),
        ManagerError::Timeout(t) => error_output(
            "analyzer_timeout",
            &format!("分析调用超时（{t}s）。工人进程未受影响，可重试。"),
        ),
        ManagerError::NotOpen { warming } => {
            if warming {
                error_output("dump_not_open", "该 dump 正在预热（MAT 建索引，GB 级需分钟级）。请稍候后重试 heap_open。")
            } else {
                error_output("dump_not_open", "该 dump 尚未打开。请先调用 heap_open(local_path)。")
            }
        }
        ManagerError::Upstream(text) => ToolOutput {
            success: false,
            data: serde_json::json!({ "upstream_is_error": true, "result": text }),
            raw_stdout: Some(text),
        },
    }
}

/// local_path 解析：相对路径以 cwd 补全 + 必须是已存在文件
fn resolve_local_path(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("local_path 不能为空".into());
    }
    let mut p = PathBuf::from(raw);
    if p.is_relative() {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        p = cwd.join(p);
    }
    if !p.is_file() {
        return Err(format!("文件不存在: {}", p.display()));
    }
    Ok(p)
}

fn heap_tool_def(
    name: &str,
    description: &str,
    schema: serde_json::Value,
    kind: HeapToolKind,
    timeouts: Timeouts,
    manager: &Arc<HeapAnalyzerManager>,
    artifacts_dir: &Path,
) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: schema,
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(HeapToolHandler {
            manager: manager.clone(),
            artifacts_dir: artifacts_dir.to_path_buf(),
            kind,
            timeouts,
        }),
    }
}

/// 注册全部 heap_* 工具（lib.rs 调用）
pub fn register_all(
    registry: &mut crate::tools::registry::ToolRegistry,
    manager: Arc<HeapAnalyzerManager>,
    artifacts_dir: PathBuf,
) {
    registry.register(heap_tool_def(
        "heap_open",
        "打开本机堆转储（.hprof）建立 MAT 分析会话并返回 heap 总览（大小/对象数/类数/GC root 数）。GB 级 dump 建索引需分钟级；jvm_heap_dump 拉回后自动预热，命中时本调用秒回。local_path 用 jvm_heap_dump / transfer_status 返回的本机路径。分析完成后建议 heap_close 释放内存。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径（transfer completed 返回的 local_path）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 600，上限 1800（大 dump 建索引慢）" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::Open,
        OPEN,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_close",
        "关闭堆转储分析会话并释放工人进程内存（MAT 索引文件保留，重开秒级）。会话结束或长期不用时调用；未打开时调用安全（幂等）。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 30，上限 60" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::Close,
        CLOSE,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_leak_suspects",
        "MAT 自动泄漏嫌疑报告（嫌疑点描述 + retained heap + 概率）。OOM 根因分析首选第一步。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::LeakSuspects,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_histogram",
        "类直方图：按类聚合的实例数 / shallow / retained heap，支持类名正则过滤与排序。定位哪类对象吃掉了内存。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "top": { "type": "number", "description": "返回条数，默认 30，上限 200" },
                "sort_by": { "type": "string", "enum": ["retained_heap", "shallow_heap", "objects"], "description": "排序键，默认 retained_heap" },
                "filter": { "type": "string", "description": "类名正则过滤（如 com\\\\.example\\\\.）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::Histogram,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_dominator_tree",
        "支配树 Top N（retained heap 最大的对象）。传 parent_object_id 进入子树下钻。定位内存根因的主要工具。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "parent_object_id": { "type": "integer", "description": "下钻父节点 objectId（来自支配树/直方图结果）；不传则返回根级 Top" },
                "top": { "type": "number", "description": "返回条数，默认 30，上限 200" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::DominatorTree,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_object_info",
        "对象详情：类 / shallow / retained / GC root 类型 / 全部字段值。object_id 来自 heap_dominator_tree / heap_histogram / heap_references 结果。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "object_id": { "type": "integer", "description": "目标对象 objectId（正整数）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path", "object_id"]
        }),
        HeapToolKind::ObjectInfo,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_path_to_gc_roots",
        "对象到 GC root 的最短引用链——确认泄漏、找出持有者的关键工具。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "object_id": { "type": "integer", "description": "目标对象 objectId（正整数）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path", "object_id"]
        }),
        HeapToolKind::PathToGcRoots,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_references",
        "对象的引用关系：direction=outbound 看它引用谁，inbound 看谁引用它（引用图下钻）。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "object_id": { "type": "integer", "description": "目标对象 objectId（正整数）" },
                "direction": { "type": "string", "enum": ["outbound", "inbound"], "description": "引用方向" },
                "top": { "type": "number", "description": "返回条数，默认 50，上限 200" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path", "object_id", "direction"]
        }),
        HeapToolKind::References,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_threads",
        "堆转储中的线程列表：retained heap + 栈帧。定位哪个线程持有大量内存（如 ThreadLocal 泄漏）。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "filter": { "type": "string", "description": "线程名正则过滤（如 http-nio）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::Threads,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::client::{HeapAnalyzerClient, MockHeapAnalyzerClient};
    use crate::analyzer::manager::{ClientFactory, ManagerConfig};
    use crate::app::events::EventBus;
    use crate::tools::registry::ToolRegistry;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SID: &str = "123e4567-e89b-12d3-a456-426614174000";

    async fn setup(
        mock: Arc<MockHeapAnalyzerClient>,
    ) -> (tempfile::TempDir, Arc<HeapAnalyzerManager>, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let spawns = Arc::new(AtomicUsize::new(0));
        let s2 = spawns.clone();
        let m2 = mock.clone();
        let factory: ClientFactory = Arc::new(move |_xmx| {
            let m2 = m2.clone();
            let s2 = s2.clone();
            Box::pin(async move {
                s2.fetch_add(1, Ordering::SeqCst);
                let c: Arc<dyn HeapAnalyzerClient> = m2;
                Ok(c)
            })
        });
        let mgr = Arc::new(HeapAnalyzerManager::new(
            factory,
            EventBus::disabled(),
            artifacts.clone(),
            ManagerConfig::default(),
        ));
        (tmp, mgr, artifacts)
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: SID.into(), channel: None }
    }

    fn dump(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "fake").unwrap();
        p
    }

    fn def<'a>(reg: &'a ToolRegistry, name: &str) -> &'a ToolDef {
        reg.get(name).unwrap()
    }

    async fn registry(mock: Arc<MockHeapAnalyzerClient>) -> (tempfile::TempDir, ToolRegistry) {
        let (tmp, mgr, artifacts) = setup(mock).await;
        let mut reg = ToolRegistry::new();
        register_all(&mut reg, mgr, artifacts);
        (tmp, reg)
    }

    #[tokio::test]
    async fn test_register_all_nine_tools_all_readonly() {
        let (tmp, reg) = registry(Arc::new(MockHeapAnalyzerClient::ok("S"))).await;
        for name in [
            "heap_open",
            "heap_close",
            "heap_leak_suspects",
            "heap_histogram",
            "heap_dominator_tree",
            "heap_object_info",
            "heap_path_to_gc_roots",
            "heap_references",
            "heap_threads",
        ] {
            let d = def(&reg, name);
            assert_eq!(d.risk_level, RiskLevel::ReadOnly, "{name}");
            assert!(!d.needs_channel, "{name}");
        }
        drop(tmp);
    }

    #[tokio::test]
    async fn test_heap_open_happy_path() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("SUMMARY"));
        let (tmp, reg) = registry(mock.clone()).await;
        let p = dump(tmp.path(), "a.hprof");
        let out = def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["tool"], "open_heap_dump");
        assert_eq!(out.data["result"], "SUMMARY");
        assert_eq!(out.data["truncated"], false);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_heap_open_missing_file_invalid_params() {
        let (tmp, reg) = registry(Arc::new(MockHeapAnalyzerClient::ok("S"))).await;
        let out = def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": "C:/definitely/nope.hprof", "session_id": SID}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_query_without_open_dump_not_open() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (tmp, reg) = registry(mock).await;
        let p = dump(tmp.path(), "a.hprof");
        let out = def(&reg, "heap_histogram")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_not_open");
        assert!(out.data["message"].as_str().unwrap().contains("heap_open"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_histogram_arg_mapping_end_to_end() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (tmp, reg) = registry(mock.clone()).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_histogram")
            .handler
            .execute(
                serde_json::json!({"local_path": p.to_string_lossy(), "top": 5, "session_id": SID}),
                &ctx(),
            )
            .await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["tool"], "get_class_histogram");
        let calls = mock.calls.lock().await;
        let (name, args) = calls.last().unwrap();
        assert_eq!(name, "get_class_histogram");
        assert_eq!(args["limit"], 5);
        assert_eq!(args["sortBy"], "RETAINED_HEAP");
        assert!(!args["id"].as_str().unwrap().is_empty());
        drop(tmp);
    }

    #[tokio::test]
    async fn test_references_invalid_direction_invalid_params() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (tmp, reg) = registry(mock).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_references")
            .handler
            .execute(
                serde_json::json!({"local_path": p.to_string_lossy(), "object_id": 1, "direction": "sideways", "session_id": SID}),
                &ctx(),
            )
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_object_info_missing_object_id_invalid_params() {
        let (tmp, reg) = registry(Arc::new(MockHeapAnalyzerClient::ok("S"))).await;
        let p = dump(tmp.path(), "a.hprof");
        let out = def(&reg, "heap_object_info")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_large_output_truncated_and_persisted() {
        let big = "x".repeat(70 * 1024);
        let mock = Arc::new(MockHeapAnalyzerClient::ok(&big));
        let (tmp, reg) = registry(mock).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_leak_suspects")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["truncated"], true);
        let full = out.data["full_output_path"].as_str().unwrap();
        assert!(std::fs::metadata(full).map(|m| m.len() as usize > 70 * 1024).unwrap_or(false));
        assert!(out.data["result"].as_str().unwrap().contains("[truncated"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_heap_close_after_open() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (tmp, reg) = registry(mock.clone()).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_close")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["was_open"], true);
        let calls = mock.calls.lock().await;
        assert!(calls.iter().any(|(n, _)| n == "close_heap_dump"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_upstream_tool_error_passthrough() {
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    Ok(crate::analyzer::client::CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Ok(crate::analyzer::client::CallOutcome { text: "MAT boom".into(), is_error: true })
                }
            }
        }));
        let (tmp, reg) = registry(mock).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_leak_suspects")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(!out.success);
        // 业务错误透传：无 error code，result 携带上游文本
        assert_eq!(out.data["error"], serde_json::Value::Null);
        assert_eq!(out.data["upstream_is_error"], true);
        assert!(out.data["result"].as_str().unwrap().contains("MAT boom"));
        drop(tmp);
    }
}
