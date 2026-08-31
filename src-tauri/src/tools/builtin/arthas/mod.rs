pub mod mapping;

use crate::arthas::manager::{ArthasManager, ManagerError};
use crate::tools::builtin::jvm::core::{clamp_or, error_output, parse_pid};
use crate::tools::builtin::run_command::{artifact_dir_for, truncate_output};
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use mapping::{ArthasToolKind, build_args, upstream_name};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// (default_secs, max_secs)
type Timeouts = (u64, u64);
const OPEN: Timeouts = (120, 300);
const CLOSE: Timeouts = (30, 60);
const FAST: Timeouts = (30, 60);
const STREAM: Timeouts = (120, 600);
const PROFILER: Timeouts = (300, 1800);

pub struct ArthasToolHandler {
    pub manager: Arc<ArthasManager>,
    pub db: sqlx::SqlitePool,
    pub artifacts_dir: PathBuf,
    pub kind: ArthasToolKind,
    pub timeouts: Timeouts,
}

#[async_trait]
impl ToolHandler for ArthasToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: environment");
        };
        let Some(pid) = args
            .get("pid")
            .and_then(|v| v.as_str())
            .and_then(|s| parse_pid(&serde_json::json!(s)))
        else {
            return error_output("invalid_params", "missing required parameter: pid（正整数字符串）");
        };
        // 按名称查环境
        let env = match crate::app::environments::find_by_name(&self.db, environment).await {
            Ok(Some(env)) => env,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    &format!(
                        "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；\
                         若无匹配，请让用户在右侧「环境」面板添加。"
                    ),
                );
            }
            Err(e) => return error_output("lookup_failed", &format!("查询环境失败: {e}")),
        };
        let timeout_secs = clamp_or(
            args.get("timeout_secs").and_then(|v| v.as_i64()),
            self.timeouts.0,
            self.timeouts.1,
        );
        let start = Instant::now();
        let label = format!("{}/{}", environment, pid);
        tracing::info!(session_id = %ctx.session_id, kind = ?self.kind, env_id = %env.id, pid, "arthas tool executing");

        match self.kind {
            ArthasToolKind::Open => {
                let java_bin = args.get("java_bin").and_then(|v| v.as_str()).unwrap_or("java");
                match self.manager.open(&ctx.session_id, &env.id, pid as i64, java_bin, timeout_secs).await {
                    Ok(outcome) => render(&ctx.session_id, &self.artifacts_dir, "arthas_open", &label, &outcome.summary, start, true).await,
                    Err(e) => self.manager_error_output(e, &ctx.session_id, "arthas_open", &label, start).await,
                }
            }
            ArthasToolKind::Close => {
                let was_open = self.manager.close(&env.id, pid as i64).await;
                ToolOutput {
                    success: true,
                    data: serde_json::json!({
                        "tool": "arthas_close",
                        "environment": environment,
                        "pid": pid,
                        "was_open": was_open,
                    }),
                    raw_stdout: None,
                }
            }
            kind => {
                let upstream = upstream_name(kind);
                let upstream_args = match build_args(kind, &args) {
                    Ok(v) => v,
                    Err(e) => return error_output("invalid_params", &e),
                };
                match self.manager.query(&env.id, pid as i64, upstream, &upstream_args, timeout_secs).await {
                    Ok(outcome) => {
                        render(&ctx.session_id, &self.artifacts_dir, upstream, &label, &outcome.text, start, !outcome.is_error).await
                    }
                    Err(e) => self.manager_error_output(e, &ctx.session_id, upstream, &label, start).await,
                }
            }
        }
    }
}

impl ArthasToolHandler {
    /// ManagerError → 结构化错误输出（对齐 heap 工具的 manager_error_output 模式）
    async fn manager_error_output(
        &self,
        e: ManagerError,
        session_id: &str,
        upstream_tool: &str,
        label: &str,
        start: Instant,
    ) -> ToolOutput {
        match e {
            ManagerError::Attach(m) => error_output("arthas_attach_failed", &m),
            ManagerError::NotOpen { attaching } => {
                if attaching {
                    error_output("arthas_not_open", "该 JVM 正在 attach 中（首次需下发工具包/建隧道，约 10-60s）。请稍候后重试。")
                } else {
                    error_output("arthas_not_open", "该 JVM 尚未 attach arthas。请先调用 arthas_open(environment, pid)。")
                }
            }
            ManagerError::Timeout(t) => error_output(
                "arthas_timeout",
                &format!("arthas 调用超时（{t}s）。会话未受影响，可加大 timeout_secs 重试。"),
            ),
            ManagerError::Transport(m) => error_output(
                "arthas_transport",
                &format!("arthas 通道传输错误：{m}。会话已失效，请重新调用 arthas_open。"),
            ),
            ManagerError::Upstream(text) => {
                render(session_id, &self.artifacts_dir, upstream_tool, label, &text, start, false).await
            }
        }
    }
}

/// 结果组装：64KB 头部截断 + 完整结果落盘 session artifacts（复用 run_command 机制）
async fn render(
    session_id: &str,
    artifacts_dir: &Path,
    upstream_tool: &str,
    label: &str,
    text: &str,
    start: Instant,
    success: bool,
) -> ToolOutput {
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let (body, truncated) = truncate_output(text);
    let session_dir = artifact_dir_for(artifacts_dir, session_id);
    let artifact_path = session_dir.join(format!("arthas-{}.md", uuid::Uuid::new_v4()));
    let full = format!(
        "--- tool: {upstream_tool} ---\n--- target: {label} ---\n--- full output ---\n{text}\n"
    );
    let mut full_output_path = None;
    match tokio::fs::create_dir_all(&session_dir).await {
        Ok(()) => {
            if tokio::fs::write(&artifact_path, &full).await.is_ok() {
                full_output_path = Some(artifact_path);
            } else {
                tracing::warn!(session_id, tool = upstream_tool, "failed to persist full arthas tool output");
            }
        }
        Err(e) => {
            tracing::warn!(session_id, tool = upstream_tool, error = %e, "failed to create artifacts dir");
        }
    }
    if success {
        tracing::info!(session_id, tool = upstream_tool, elapsed_ms, truncated, "arthas tool executed");
    } else {
        tracing::warn!(session_id, tool = upstream_tool, elapsed_ms, truncated, "arthas tool upstream error passthrough");
    }
    let mut data = serde_json::json!({
        "tool": upstream_tool,
        "target": label,
        "elapsed_ms": elapsed_ms,
        "output": body,
        "truncated": truncated,
    });
    if let Some(p) = full_output_path {
        data["full_output_path"] = serde_json::json!(p.display().to_string());
    }
    ToolOutput { success, data, raw_stdout: None }
}

/// 注册全部 27 个 arthas 工具
pub fn register_all(
    registry: &mut crate::tools::registry::ToolRegistry,
    manager: Arc<ArthasManager>,
    db: sqlx::SqlitePool,
    artifacts_dir: PathBuf,
) {
    // (name, description, risk, timeouts, kind)
    let defs: Vec<(&str, &str, RiskLevel, Timeouts, ArthasToolKind)> = vec![
        ("arthas_open",
         "attach arthas 到目标 JVM 并建立诊断通道（幂等，已 attach 秒回）。首次自动下发 arthas 工具包（需 Artifactory 已配置）；SSH 用户与 JVM 用户不一致时需要已录入对应用户凭证。加载 agent 侵入目标 JVM，需确认。",
         RiskLevel::Low, OPEN, ArthasToolKind::Open),
        ("arthas_close",
         "停止目标 JVM 上的 arthas agent 并释放通道（卸载字节码增强与 agent，幂等）。诊断完成后调用，或留给空闲自动回收。",
         RiskLevel::ReadOnly, CLOSE, ArthasToolKind::Close),
        ("arthas_dashboard",
         "实时 JVM 面板：线程/内存/GC/运行环境概览。args: {interval?, num?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Dashboard),
        ("arthas_jvm",
         "JVM 详细运行时信息（类加载/编译器/GC/线程/系统属性概览）。args: {}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Jvm),
        ("arthas_memory",
         "JVM 内存使用：各分代/元空间/堆外。args: {}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Memory),
        ("arthas_sysenv",
         "查看目标 JVM 进程环境变量。args: {variable?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Sysenv),
        ("arthas_perfcounter",
         "JVM Perf Counter 性能计数器信息。args: {}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Perfcounter),
        ("arthas_sc",
         "搜索 JVM 已加载类，可看类详情（类加载器/父类/接口/字段）。args: {classPattern, details?, fields?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Sc),
        ("arthas_sm",
         "搜索已加载类的方法信息（签名/参数/注解）。args: {classPattern, methodPattern?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Sm),
        ("arthas_jad",
         "反编译指定已加载类（JVM 实际运行的字节码 → Java 源码）。args: {classPattern, methodName?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Jad),
        ("arthas_classloader",
         "ClassLoader 诊断：统计/继承树/加载的 URL。args: {}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Classloader),
        ("arthas_getstatic",
         "查看类的静态字段值。args: {className, field?, classloader?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Getstatic),
        ("arthas_mbean",
         "查看/监控 MBean 属性信息。args: {name, attribute?, interval?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Mbean),
        ("arthas_dump",
         "导出指定类（已加载字节码）到目标机 arthas-output 目录，配合 arthas_viewfile/文件传输查看。args: {classPattern}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Dump),
        ("arthas_thread",
         "线程信息与堆栈：定位 BLOCKED/死锁/最忙线程。不支持 interrupt 子操作。args: {id?, state?, topN?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Thread),
        ("arthas_viewfile",
         "查看目标机 arthas-output 目录内文件（profiler 火焰图等）。args: {file, cursor?, offset?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Viewfile),
        ("arthas_options",
         "查看 arthas 全局开关选项。args: {option?, value?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Options),
        ("arthas_watch",
         "观察方法执行的入参/返回值/异常（实时，字节码增强）。args: {classPattern, methodPattern, express?, condition?}",
         RiskLevel::Low, STREAM, ArthasToolKind::Watch),
        ("arthas_trace",
         "追踪方法内部调用链与各级耗时，定位慢调用。args: {classPattern, methodPattern, condition?}",
         RiskLevel::Low, STREAM, ArthasToolKind::Trace),
        ("arthas_stack",
         "输出方法被调用的调用路径（谁调用了它）。args: {classPattern, methodPattern}",
         RiskLevel::Low, STREAM, ArthasToolKind::Stack),
        ("arthas_monitor",
         "监控方法调用统计：次数/成功率/平均 RT（周期采样）。args: {classPattern, methodPattern, interval?}",
         RiskLevel::Low, STREAM, ArthasToolKind::Monitor),
        ("arthas_tt",
         "方法执行数据时空隧道：记录每次调用的入参/返回，可事后查看/重放。args: {classPattern, methodPattern, ...}",
         RiskLevel::Low, STREAM, ArthasToolKind::Tt),
        ("arthas_ognl",
         "执行 OGNL 表达式（可调用方法/读写字段，能力很强，需确认）。args: {express, classloader?}",
         RiskLevel::Low, FAST, ArthasToolKind::Ognl),
        ("arthas_vmtool",
         "VM 工具集：forceGc（强制 GC）/ getInstances（获取类实例）。不支持 interrupt。args: {action, className?, limit?}",
         RiskLevel::Low, FAST, ArthasToolKind::Vmtool),
        ("arthas_sysprop",
         "查看/修改目标 JVM 系统属性（可写，需确认）。args: {name?, value?}",
         RiskLevel::Low, FAST, ArthasToolKind::Sysprop),
        ("arthas_vmoption",
         "查看/更新目标 JVM VM 选项（可写，需确认）。args: {name?, value?}",
         RiskLevel::Low, FAST, ArthasToolKind::Vmoption),
        ("arthas_profiler",
         "async-profiler 采样：CPU/alloc/lock，输出火焰图（到 arthas-output 目录，用 arthas_viewfile 或文件传输查看）。采样周期长，注意 timeout_secs。args: {action, event?, duration?}",
         RiskLevel::Low, PROFILER, ArthasToolKind::Profiler),
    ];
    for (name, desc, risk, timeouts, kind) in defs {
        registry.register(arthas_tool_def(name, desc, risk, timeouts, kind, manager.clone(), db.clone(), artifacts_dir.clone()));
    }
}

fn arthas_tool_def(
    name: &str,
    description: &str,
    risk: RiskLevel,
    timeouts: Timeouts,
    kind: ArthasToolKind,
    manager: Arc<ArthasManager>,
    db: sqlx::SqlitePool,
    artifacts_dir: PathBuf,
) -> ToolDef {
    let mut props = serde_json::json!({
        "environment": { "type": "string", "description": "目标环境名（来自 list_environments）" },
        "pid": { "type": "string", "description": "目标 JVM 进程号（来自 list_processes）" },
        "timeout_secs": { "type": "integer", "description": format!("超时秒数，默认 {}，最大 {}", timeouts.0, timeouts.1) },
    });
    if !matches!(kind, ArthasToolKind::Open | ArthasToolKind::Close) {
        props["args"] = serde_json::json!({
            "type": "object",
            "description": "arthas 命令参数对象（字段与 arthas 命令选项一致，原样透传给 arthas）"
        });
    }
    if matches!(kind, ArthasToolKind::Open) {
        props["java_bin"] = serde_json::json!({
            "type": "string",
            "description": "目标机 java 可执行文件路径（默认 java；目标机 PATH 无 java 时需指定）"
        });
    }
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": props,
            "required": ["environment", "pid"],
        }),
        risk_level: risk,
        needs_channel: false,
        handler: Arc::new(ArthasToolHandler {
            manager,
            db,
            artifacts_dir,
            kind,
            timeouts,
        }),
    }
}
