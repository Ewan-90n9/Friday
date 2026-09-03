pub mod mapping;

use crate::app::events::{AppEvent, EventBus};
use crate::exec::channel::ExecChannel;
use crate::jfr::{JmcError, JmcManager};
use crate::tools::builtin::jvm::core::{
    clamp_or, error_output, is_jdk_missing, parse_pid, require_bins, resolve_environment,
    JvmExecCore,
};
use crate::tools::builtin::run_command::{artifact_dir_for, truncate_output};
use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// (default_secs, max_secs)
type Timeouts = (u64, u64);
const QUERY: Timeouts = (60, 300);
const HEAVY: Timeouts = (300, 1800);

/// 录制落盘轮询间隔（虚拟时钟友好，测试 start_paused 可瞬时推进）
const RECORD_POLL_INTERVAL_SECS: u64 = 3;

/// jfr_record：一次性定时录制 + 后台拉回
pub struct JfrRecordHandler {
    pub core: Arc<JvmExecCore>,
    pub bus: EventBus,
    pub transfer: Arc<crate::transfer::TransferManager>,
}

/// jfr_compare / 代理分析工具
pub struct JfrProxyHandler {
    pub jmc: Arc<JmcManager>,
    pub artifacts_dir: PathBuf,
    pub kind: JfrToolKind,
    pub timeouts: Timeouts,
}

#[derive(Debug, Clone, Copy)]
pub enum JfrToolKind {
    Compare,
    Proxy(mapping::JfrProxyKind),
}

#[async_trait]
impl ToolHandler for JfrRecordHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.execute_record(&args, ctx).await
    }
}

#[async_trait]
impl ToolHandler for JfrProxyHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        match self.kind {
            JfrToolKind::Compare => self.execute_compare(&args, ctx).await,
            JfrToolKind::Proxy(kind) => self.execute_proxy(kind, &args, ctx).await,
        }
    }
}

impl JfrRecordHandler {
    async fn execute_record(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_args", "missing required parameter: environment");
        };
        let Some(pid) = args.get("pid").and_then(|v| parse_pid(v)) else {
            return error_output("invalid_args", "pid 必须是正整数字符串");
        };
        let (duration_secs, settings) = match mapping::validate_record_params(args) {
            Ok(v) => v,
            Err(e) => return error_output("invalid_args", &e),
        };
        let timeout_secs = mapping::effective_record_timeout(
            args.get("timeout_secs").and_then(|v| v.as_i64()),
            duration_secs,
        );

        let (env, channel) = match resolve_environment(&self.core.db, &self.core.exec_pool, environment).await {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    &format!(
                        "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"
                    ),
                );
            }
            Err(e) => return error_output("connection_error", &e),
        };

        // JDK 路径：查缓存，miss 引导 ensure_tool
        let Some(layout) = self.core.jdk_cache.get(&env.id).await else {
            tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, "jdk not provisioned (cache miss)");
            return error_output(
                "jdk_not_provisioned",
                "该环境尚未装备 JDK。请先调用 ensure_tool(environment, tool=\"jdk\") 装备，然后重试本工具。",
            );
        };
        let bins = match require_bins(&layout, &["jcmd"]) {
            Ok(b) => b,
            Err(e) => return error_output("jdk_not_provisioned", &e),
        };
        let jcmd = &bins[0];

        // ① 一次性定时录制（文件名 Friday 固定构造——不开放自定义，注入面）
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let remote_path = format!("/tmp/friday-tools/recording-{pid}-{ts}.jfr");
        let name = format!("friday-{ts}");
        let start_cmd = mapping::jfr_start_command(jcmd, pid, &name, duration_secs, &settings, &remote_path);

        tracing::info!(session_id = %ctx.session_id, env_id = %env.id, pid, command = %start_cmd, "jfr record: starting");
        self.emit_progress(
            &ctx.session_id,
            "record",
            &format!("JFR 录制已启动（{duration_secs}s，settings={settings}），等待落盘…"),
        );

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        let start_timeout = timeout_secs.min(120);
        let start_output = match tokio::time::timeout(
            std::time::Duration::from_secs(start_timeout),
            channel.run(&start_cmd),
        )
        .await
        {
            Err(_) => {
                tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, timeout_secs = start_timeout, "JFR.start timed out, dropping connection");
                {
                    let mut pool = self.core.exec_pool.lock().await;
                    pool.disconnect(&env.id).await;
                }
                return error_output(
                    "timeout_error",
                    &format!("JFR.start 超时（{start_timeout}s）；ssh 连接已断开"),
                );
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "JFR.start exec failed");
                return error_output("connection_error", &e.to_string());
            }
            Ok(Ok(output)) => {
                if is_jdk_missing(output.exit_code, &output.stderr) {
                    tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, "jdk missing on remote, clearing cache");
                    self.core.jdk_cache.clear(&env.id).await;
                    return error_output(
                        "jdk_missing_on_remote",
                        "远端 JDK 已不存在（可能 /tmp 被清理）。请重新调用 ensure_tool 装备后重试。",
                    );
                }
                if output.exit_code != 0 {
                    // JFR.start 失败：透传 jcmd 输出 + 兼容性提示（JDK 8 场景）
                    tracing::error!(session_id = %ctx.session_id, env_id = %env.id, exit_code = output.exit_code, "JFR.start command failed");
                    return ToolOutput {
                        success: false,
                        data: serde_json::json!({
                            "error": "record_failed",
                            "message": "JFR.start 失败。目标 JVM 兼容性：JDK 11+ 开箱即用；Oracle JDK 8 需启动参数 -XX:+UnlockCommercialVMOption -XX:+FlightRecorder；OpenJDK 8 无 JFR——此类场景改用 arthas_profiler。",
                            "stdout": output.stdout,
                            "stderr": output.stderr,
                            "exit_code": output.exit_code,
                        }),
                        raw_stdout: Some(output.stdout),
                    };
                }
                output
            }
        };

        // ② 等待录制落盘：duration 到期 + 文件大小稳定（两次轮询相等且非零）
        let remote_size = match wait_for_recording(&channel, &remote_path, duration_secs, deadline).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, remote_path, "recording file never materialized");
                return error_output("record_not_found", &e);
            }
        };

        // ③ 后台拉回：TransferManager（MCP 同步调用返回，Agent 轮询 transfer_status）
        let session_dir = artifact_dir_for(&self.core.artifacts_dir, &ctx.session_id);
        let local_path = session_dir.join(format!("recording-{pid}-{ts}.jfr"));
        let state = crate::transfer::state::TransferState::new(
            crate::transfer::state::Direction::Download,
            &ctx.session_id,
            &env.id,
            &remote_path,
            local_path.clone(),
            true, // 下载成功后清理远端（Friday 自己生成的文件）
        );
        let transfer_id = self.transfer.start(state).await;

        self.emit_progress(
            &ctx.session_id,
            "download",
            "录制完成，后台拉回已启动（轮询 transfer_status 获取进度）",
        );

        tracing::info!(
            session_id = %ctx.session_id, env_id = %env.id, pid,
            transfer_id = %transfer_id,
            remote_path, remote_size, duration_secs, settings,
            "jfr recording complete, background download started"
        );

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "transfer_id": transfer_id,
                "remote_path": remote_path,
                "remote_size": remote_size,
                "duration_secs": duration_secs,
                "settings": settings,
                "local_path": local_path.to_string_lossy(),
                "note": "JFR 录制完成，正在后台拉回。请轮询 transfer_status(transfer_id)；completed 后自动预热 JMC 分析，用 jfr_quick_analysis(local_path) / jfr_rules(local_path) 起步诊断；failed 时远端文件保留，可用 file_download 重试（断点续传）。",
            }),
            raw_stdout: Some(start_output.stdout),
        }
    }

    fn emit_progress(&self, session_id: &str, stage: &str, detail: &str) {
        self.bus.emit(
            session_id,
            AppEvent::ProvisionProgress {
                session_id: session_id.to_string(),
                tool: "jfr_record".to_string(),
                stage: stage.to_string(),
                detail: detail.to_string(),
            },
        );
    }
}

impl JfrProxyHandler {
    async fn execute_compare(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let baseline_raw = args.get("baseline_local_path").and_then(|v| v.as_str());
        let target_raw = args.get("target_local_path").and_then(|v| v.as_str());
        let (Some(baseline_raw), Some(target_raw)) = (baseline_raw, target_raw) else {
            return error_output(
                "invalid_args",
                "missing required parameters: baseline_local_path / target_local_path（两次录制各一份）",
            );
        };
        let baseline = match resolve_existing_file(baseline_raw) {
            Ok(p) => p,
            Err(e) => return error_output("invalid_path", &e),
        };
        let target = match resolve_existing_file(target_raw) {
            Ok(p) => p,
            Err(e) => return error_output("invalid_path", &e),
        };
        let resolved = format!("{} -> {}", baseline.display(), target.display());
        let timeout_secs =
            clamp_or(args.get("timeout_secs").and_then(|v| v.as_i64()), self.timeouts.0, self.timeouts.1);
        let (upstream, upstream_args) = mapping::build_compare(
            &baseline.to_string_lossy(),
            &target.to_string_lossy(),
            args.get("args"),
        );
        self.run_query(&upstream, &upstream_args, &resolved, timeout_secs, ctx).await
    }

    async fn execute_proxy(
        &self,
        kind: mapping::JfrProxyKind,
        args: &serde_json::Value,
        ctx: &ToolContext,
    ) -> ToolOutput {
        let Some(local_path) = args.get("local_path").and_then(|v| v.as_str()) else {
            return error_output("invalid_args", "missing required parameter: local_path");
        };
        let path = match resolve_existing_file(local_path) {
            Ok(p) => p,
            Err(e) => return error_output("invalid_path", &e),
        };
        let resolved = path.display().to_string();
        let timeout_secs =
            clamp_or(args.get("timeout_secs").and_then(|v| v.as_i64()), self.timeouts.0, self.timeouts.1);
        let (upstream, upstream_args) = mapping::build_proxy(kind, &resolved, args.get("args"));
        self.run_query(&upstream, &upstream_args, &resolved, timeout_secs, ctx).await
    }

    async fn run_query(
        &self,
        upstream: &str,
        upstream_args: &serde_json::Value,
        resolved_path: &str,
        timeout_secs: u64,
        ctx: &ToolContext,
    ) -> ToolOutput {
        let start = std::time::Instant::now();
        tracing::info!(session_id = %ctx.session_id, upstream = %upstream, jfr = %resolved_path, timeout_secs, "jfr tool executing");
        match self.jmc.query(upstream, upstream_args, timeout_secs).await {
            Ok(outcome) => {
                render(&ctx.session_id, &self.artifacts_dir, upstream, resolved_path, &outcome.text, start, true)
                    .await
            }
            Err(e) => {
                tracing::warn!(session_id = %ctx.session_id, upstream = %upstream, error = %e, "jfr tool failed");
                self.jmc_error_output(e, &ctx.session_id, upstream, resolved_path, start)
                    .await
            }
        }
    }

    /// JmcError → 结构化错误输出。Upstream（JMC 业务错误）走透传（无 error code，
    /// 对齐 heap_*/jvm_* 惯例），但同样经过 64KB 截断 + 完整结果落盘路径。
    async fn jmc_error_output(
        &self,
        e: JmcError,
        session_id: &str,
        upstream_tool: &str,
        local_path: &str,
        start: std::time::Instant,
    ) -> ToolOutput {
        match e {
            JmcError::JavaMissing(m) => error_output(
                "java_missing",
                &format!("本机 Java 21+ 不可用：{m}。请安装 JDK 21+ 后重试。"),
            ),
            JmcError::Unavailable(m) => error_output(
                "jmc_unavailable",
                &format!("{m}。可重试一次；连续失败请查看 Friday 日志。"),
            ),
            JmcError::Timeout(t) => error_output(
                "jmc_timeout",
                &format!("JMC 分析调用超时（{t}s）。工人进程未受影响，可加大 timeout_secs 或用 start_time/end_time 缩小时间窗后重试。"),
            ),
            JmcError::Upstream(text) => {
                render(session_id, &self.artifacts_dir, upstream_tool, local_path, &text, start, false).await
            }
        }
    }
}

/// 等待录制落盘：duration 到期后文件存在（size > 0）且两次轮询大小相等 → 稳定。
/// deadline 用尽 → Err（附远端路径与已等待时长）。
/// 全程 tokio 虚拟时钟友好（测试 start_paused 瞬时推进）。
async fn wait_for_recording(
    channel: &Arc<dyn ExecChannel>,
    remote_path: &str,
    duration_secs: u32,
    deadline: tokio::time::Instant,
) -> Result<u64, String> {
    let start = tokio::time::Instant::now();
    let mut last_size: u64 = 0;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "录制到时后文件未就绪：{remote_path}（已等待 {}s）。远端文件可能仍在写入，可稍后用 file_download 手动拉回",
                start.elapsed().as_secs()
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(RECORD_POLL_INTERVAL_SECS)).await;
        let stat_cmd = format!("stat -c %s {remote_path}");
        let size: u64 = match channel.run(&stat_cmd).await {
            Ok(o) if o.exit_code == 0 => o.stdout.trim().parse().unwrap_or(0),
            _ => 0,
        };
        let elapsed = start.elapsed().as_secs();
        if elapsed >= duration_secs as u64 && size > 0 && size == last_size {
            return Ok(size);
        }
        last_size = size;
    }
}

/// 结果组装：64KB 头部截断 + 完整结果落盘 session artifacts（复用 run_command 机制）。
/// success=false 用于上游业务错误透传（upstream_is_error 标记，无 error code）。
async fn render(
    session_id: &str,
    artifacts_dir: &Path,
    upstream_tool: &str,
    local_path: &str,
    text: &str,
    start: std::time::Instant,
    success: bool,
) -> ToolOutput {
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let (body, truncated) = truncate_output(text);
    let session_dir = artifact_dir_for(artifacts_dir, session_id);
    let artifact_path = session_dir.join(format!("jfr-{}.md", uuid::Uuid::new_v4()));
    let full = format!("--- tool: {upstream_tool} ---\n--- local_path: {local_path} ---\n--- full output ---\n{text}\n");
    let mut full_output_path = None;
    match tokio::fs::create_dir_all(&session_dir).await {
        Ok(()) => {
            if tokio::fs::write(&artifact_path, &full).await.is_ok() {
                full_output_path = Some(artifact_path);
            } else {
                tracing::warn!(session_id, tool = upstream_tool, "failed to persist full jfr tool output");
            }
        }
        Err(e) => {
            tracing::warn!(session_id, tool = upstream_tool, error = %e, "failed to create artifacts dir");
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
    if success {
        tracing::info!(session_id, tool = upstream_tool, elapsed_ms, truncated, "jfr tool executed");
    } else {
        tracing::warn!(session_id, tool = upstream_tool, elapsed_ms, truncated, "jfr tool upstream error passthrough");
    }
    let mut data = serde_json::json!({
        "tool": upstream_tool,
        "local_path": local_path,
        "result": result_field,
        "elapsed_ms": elapsed_ms,
        "truncated": truncated,
        "full_output_path": full_output_path.as_ref().map(|p| p.display().to_string()),
    });
    if !success {
        data["upstream_is_error"] = serde_json::json!(true);
    }
    ToolOutput {
        success,
        data,
        raw_stdout: Some(text.to_string()),
    }
}

/// local_path 解析：相对路径以 cwd 补全 + 必须是已存在文件。
fn resolve_existing_file(raw: &str) -> Result<PathBuf, String> {
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

fn record_tool_def(
    core: &Arc<JvmExecCore>,
    bus: &EventBus,
    transfer: &Arc<crate::transfer::TransferManager>,
) -> ToolDef {
    ToolDef {
        name: "jfr_record".to_string(),
        description: "对目标 JVM 热开启 JFR 飞行录制并后台拉回（jcmd JFR.start，目标需 JDK 11+，profile 档开销约 1~3%，不中断服务）。一次性定时录制 duration_secs 秒（10~600，默认 60）后自动落盘 → 后台拉回（返回 transfer_id，轮询 transfer_status）→ completed 后自动预热 JMC 分析，直接用 jfr_quick_analysis / jfr_rules 起步。⚠ 目标 JDK 8 不支持热开启 JFR（Oracle JDK 8 需启动参数，OpenJDK 8 无 JFR），此类场景改用 arthas_profiler。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                "pid": { "type": "string", "description": "目标 Java 进程 PID（list_processes 返回）" },
                "duration_secs": { "type": "number", "description": "录制时长秒数，10~600，默认 60" },
                "settings": { "type": "string", "enum": ["profile", "default"], "description": "事件档位：profile 全维度（开销 1~3%），default 低开销（<1%），默认 profile" },
                "timeout_secs": { "type": "number", "description": "总超时秒数（含录制等待与落盘轮询），默认 600，上限 1800；实际下限为 duration_secs+120" }
            },
            "required": ["environment", "pid"]
        }),
        risk_level: RiskLevel::Low,
        category: ToolCategory::Jfr,
        needs_channel: false,
        handler: Arc::new(JfrRecordHandler {
            core: core.clone(),
            bus: bus.clone(),
            transfer: transfer.clone(),
        }),
    }
}

fn proxy_tool_def(
    name: &str,
    description: &str,
    kind: JfrToolKind,
    timeouts: Timeouts,
    jmc: &Arc<JmcManager>,
    artifacts_dir: &Path,
) -> ToolDef {
    let schema = match kind {
        JfrToolKind::Compare => serde_json::json!({
            "type": "object",
            "properties": {
                "baseline_local_path": { "type": "string", "description": "基准录制（如正常期）的本机路径" },
                "target_local_path": { "type": "string", "description": "对比录制（如故障期）的本机路径" },
                "args": { "type": "object", "description": "上游选项透传（start_time/end_time 等）" },
                "timeout_secs": { "type": "number", "description": format!("超时秒数，默认 {}，上限 {}", timeouts.0, timeouts.1) }
            },
            "required": ["baseline_local_path", "target_local_path"]
        }),
        JfrToolKind::Proxy(_) => serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "本机 JFR 录制文件绝对路径（jfr_record 返回的 local_path 或用户已有文件）" },
                "args": { "type": "object", "description": "上游分析选项透传（如 top_n / thread_name / package_prefix / focus / class_pattern / start_time / end_time，见工具描述）" },
                "timeout_secs": { "type": "number", "description": format!("超时秒数，默认 {}，上限 {}", timeouts.0, timeouts.1) }
            },
            "required": ["local_path"]
        }),
    };
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: schema,
        risk_level: RiskLevel::ReadOnly,
        category: ToolCategory::Jfr,
        needs_channel: false,
        handler: Arc::new(JfrProxyHandler {
            jmc: jmc.clone(),
            artifacts_dir: artifacts_dir.to_path_buf(),
            kind,
            timeouts,
        }),
    }
}

/// 注册全部 jfr_* 工具（lib.rs 调用）：1 录制 + 20 代理 + 1 对比
pub fn register_all(
    registry: &mut crate::tools::registry::ToolRegistry,
    jmc: Arc<JmcManager>,
    core: Arc<JvmExecCore>,
    bus: EventBus,
    transfer: Arc<crate::transfer::TransferManager>,
    artifacts_dir: PathBuf,
) {
    registry.register(record_tool_def(&core, &bus, &transfer));

    // (Friday 名, 描述, 代理类型, 超时档)
    let proxies: &[(&str, &str, mapping::JfrProxyKind, Timeouts)] = &[
        ("jfr_overview", "JFR 录制总览：录制时长、事件数、JVM/系统信息。分析起点（jfr_record 完成预热后秒回）。args 可选：start_time/end_time。", mapping::JfrProxyKind::Overview, QUERY),
        ("jfr_rules", "JMC 规则引擎自动瓶颈检测（GC/内存/CPU/锁/IO 规则，带严重度与建议）。录制体检首选。args 可选：min_severity/start_time/end_time。", mapping::JfrProxyKind::Rules, QUERY),
        ("jfr_quick_analysis", "一键宏诊断仪表盘：自动检测主瓶颈并按严重度分类（CPU/内存/锁/IO）。性能问题第一步。args 可选：focus（cpu/memory/locks/io）/start_time/end_time。", mapping::JfrProxyKind::QuickAnalysis, HEAVY),
        ("jfr_gc_detail", "GC 深度分析：分阶段暂停耗时、GC cause 分布、堆趋势、GC 配置。args 可选：detail_level/start_time/end_time。", mapping::JfrProxyKind::GcDetail, QUERY),
        ("jfr_memory_leaks", "老对象采样泄漏分析：按类统计存活老对象（JFR 对象采样），定位疑似泄漏类；与 heap_*（MAT）互补。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::MemoryLeaks, HEAVY),
        ("jfr_predictive_leak", "数学检测内存泄漏：对 post-GC 堆使用做线性回归（r_squared 拟合度），泄漏趋势确认。args 可选：r_squared_threshold/start_time/end_time。", mapping::JfrProxyKind::PredictiveLeak, HEAVY),
        ("jfr_allocation_hotspots", "内存分配热点：按类和分配调用点统计分配速率，定位分配风暴。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::AllocationHotspots, QUERY),
        ("jfr_hot_methods", "CPU 热点方法 Top N（执行采样）。args 可选：top_n/thread_name/package_prefix/start_time/end_time。", mapping::JfrProxyKind::HotMethods, QUERY),
        ("jfr_thread_cpu", "线程级 CPU 消耗排名（执行采样）。args 可选：top_n/package_prefix/start_time/end_time。", mapping::JfrProxyKind::ThreadCpu, QUERY),
        ("jfr_cpu_flame", "CPU 火焰图数据：热点调用路径 + 线程状态。args 可选：top_n/package_prefix/start_time/end_time。", mapping::JfrProxyKind::CpuFlame, HEAVY),
        ("jfr_thread_contention", "锁竞争分析：monitor 阻塞/挂起/等待统计。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::ThreadContention, QUERY),
        ("jfr_deadlock_detection", "死锁环检测：monitor 持有/等待关系分析。args 可选：start_time/end_time。", mapping::JfrProxyKind::DeadlockDetection, QUERY),
        ("jfr_io_hotspots", "IO 热点：慢/高频文件与 socket 操作（按路径/主机），含调用点。args 可选：io_type/top_n/start_time/end_time。", mapping::JfrProxyKind::IoHotspots, QUERY),
        ("jfr_exceptions", "异常抛出统计：按异常类统计次数与栈。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::Exceptions, QUERY),
        ("jfr_errors", "严重错误分析：OutOfMemoryError/StackOverflowError 等按严重度分类。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::Errors, QUERY),
        ("jfr_safepoints", "safepoint 分析：GC 外 STW 暂停（vm operation 耗时），延迟毛刺定位。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::Safepoints, QUERY),
        ("jfr_virtual_threads", "虚拟线程分析：pinning 位点与执行失败（目标 JDK 21+）。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::VirtualThreads, QUERY),
        ("jfr_stack_trace_search", "跨 13 类事件全栈正则搜索（非截断栈）。找人/找路径利器。args 必填：class_pattern；可选 event_type/limit/start_time/end_time。", mapping::JfrProxyKind::StackTraceSearch, HEAVY),
        ("jfr_correlate", "跨维度相关性引擎：锁↔IO↔热点方法关联成瓶颈链。args 可选：dimension/top_n/start_time/end_time。", mapping::JfrProxyKind::Correlate, HEAVY),
        ("jfr_request_waterfall", "线程时序瀑布：按时间顺序串联 锁→IO→CPU→异常 事件。args 必填：thread_name；可选 max_events/start_time/end_time。", mapping::JfrProxyKind::RequestWaterfall, HEAVY),
    ];
    for (name, desc, kind, timeouts) in proxies {
        registry.register(proxy_tool_def(
            name,
            desc,
            JfrToolKind::Proxy(*kind),
            *timeouts,
            &jmc,
            &artifacts_dir,
        ));
    }

    registry.register(proxy_tool_def(
        "jfr_compare",
        "两个 JFR 录制的 A/B 对比（优化前后、故障期 vs 正常期）：事件量/热点/暂停等维度差异汇总。",
        JfrToolKind::Compare,
        HEAVY,
        &jmc,
        &artifacts_dir,
    ));
}

#[cfg(test)]
mod tests {
    use super::register_all;
    use crate::app::events::EventBus;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::jfr::client::MockJmcClient;
    use crate::jfr::manager::{ClientFactory, JmcConfig, JmcManager};
    use crate::tools::builtin::jvm::core::JvmExecCore;
    use crate::tools::builtin::jvm::jdk_cache::JdkLayout;
    use crate::tools::category::ToolCategory;
    use crate::tools::registry::{ToolContext, ToolRegistry};
    use crate::tools::risk::RiskLevel;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    const SID: &str = "123e4567-e89b-12d3-a456-426614174000";

    /// JFR 感知的可编程 mock channel（对齐 heap_dump.rs 的 DumpChannel 模式）
    struct JfrChannel {
        start_exit: i32,
        stat_size: &'static str,
        calls: TokioMutex<Vec<String>>,
    }

    #[async_trait]
    impl ExecChannel for JfrChannel {
        async fn run(
            &self,
            cmd: &str,
        ) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.lock().await.push(cmd.to_string());
            if cmd.contains("JFR.start") {
                return Ok(ExecOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: self.start_exit,
                });
            }
            if cmd.starts_with("stat -c %s") {
                return Ok(ExecOutput {
                    stdout: self.stat_size.to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                });
            }
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool {
            true
        }
    }

    async fn setup(channel: Arc<dyn ExecChannel>) -> (tempfile::TempDir, Arc<JvmExecCore>, Arc<crate::transfer::TransferManager>) {
        // 注意：调用方在 setup 完成后才 pause 时钟（而非 start_paused 全程暂停）——
        // sqlx 建新连接走真实 IO，而 pool acquire_timeout 是 tokio 定时器，全程暂停的
        // auto-advance 会在真实连接完成前把时钟推到超时点（PoolTimedOut，且嵌套
        // runtime 建 pool 会产生随其销毁的僵尸连接）。
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        let env_id = crate::app::env_save::save_environment(
            &db,
            None,
            "prod",
            "10.0.0.1",
            22,
            vec![crate::app::env_save::CredentialInput {
                id: None,
                username: "root".to_string(),
                auth_type: "password".to_string(),
                private_key_path: None,
                secret: None,
                is_default: true,
            }],
        )
        .await
        .unwrap()
        .environment
        .id;
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        exec_pool.lock().await.insert_channel(env_id.clone(), channel).await;
        let mut bins = HashMap::new();
        bins.insert("jcmd".to_string(), "/tmp/jdk/bin/jcmd".to_string());
        let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
        jdk_cache
            .set(&env_id, JdkLayout { tool_home: "/tmp/jdk".into(), bins })
            .await;
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let core = Arc::new(JvmExecCore {
            db: db.clone(),
            exec_pool,
            jdk_cache,
            artifacts_dir: artifacts.clone(),
        });
        let mgr = Arc::new(crate::transfer::TransferManager::new(db, EventBus::disabled()));
        (tmp, core, mgr)
    }

    fn jmc_manager(mock: Arc<MockJmcClient>) -> Arc<JmcManager> {
        let factory: ClientFactory = Arc::new(move || {
            let m = mock.clone();
            Box::pin(async move { Ok(m as Arc<dyn crate::jfr::client::JmcClient>) })
        });
        Arc::new(JmcManager::new(factory, EventBus::disabled(), JmcConfig::default()))
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: SID.into(), channel: None }
    }

    fn def<'a>(reg: &'a ToolRegistry, name: &str) -> &'a crate::tools::registry::ToolDef {
        reg.get(name).unwrap()
    }

    async fn registry(
        channel: Arc<dyn ExecChannel>,
        mock: Arc<MockJmcClient>,
    ) -> (tempfile::TempDir, ToolRegistry) {
        let (tmp, core, transfer) = setup(channel).await;
        let mut reg = ToolRegistry::new();
        register_all(
            &mut reg,
            jmc_manager(mock),
            core,
            EventBus::disabled(),
            transfer,
            tmp.path().join("artifacts"),
        );
        (tmp, reg)
    }

    fn jfr_file(dir: &std::path::Path) -> std::path::PathBuf {
        let p = dir.join("a.jfr");
        std::fs::write(&p, "fake jfr").unwrap();
        p
    }

    fn std_channel(stat: &'static str) -> Arc<JfrChannel> {
        Arc::new(JfrChannel { start_exit: 0, stat_size: stat, calls: TokioMutex::new(Vec::new()) })
    }

    /// 虚拟时钟起搏器：常驻 1ms 定时任务，把 auto-advance 的推进粒度钳制在 1ms。
    /// 无起搏时，runtime 空闲即跳到下一个 pending 定时器——sqlx 30s acquire 超时
    /// 定时器会在其真实 IO（连接归还/查询往返，µs 级）完成前被瞬间穿透（PoolTimedOut）。
    /// 用后 abort。
    fn spawn_auto_advance_pacer() -> tokio::task::JoinHandle<()> {
        tokio::spawn(async {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
    }

    #[tokio::test]
    async fn test_register_all_twenty_two_tools() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        let expected = [
            "jfr_record",
            "jfr_overview",
            "jfr_rules",
            "jfr_quick_analysis",
            "jfr_gc_detail",
            "jfr_memory_leaks",
            "jfr_predictive_leak",
            "jfr_allocation_hotspots",
            "jfr_hot_methods",
            "jfr_thread_cpu",
            "jfr_cpu_flame",
            "jfr_thread_contention",
            "jfr_deadlock_detection",
            "jfr_io_hotspots",
            "jfr_exceptions",
            "jfr_errors",
            "jfr_safepoints",
            "jfr_virtual_threads",
            "jfr_stack_trace_search",
            "jfr_correlate",
            "jfr_request_waterfall",
            "jfr_compare",
        ];
        assert_eq!(expected.len(), 22);
        for name in expected {
            let d = def(&reg, name);
            assert_eq!(d.category, ToolCategory::Jfr, "{name}");
            assert!(!d.needs_channel, "{name}");
        }
        assert_eq!(def(&reg, "jfr_record").risk_level, RiskLevel::Low);
        assert_eq!(def(&reg, "jfr_overview").risk_level, RiskLevel::ReadOnly);
        assert_eq!(def(&reg, "jfr_compare").risk_level, RiskLevel::ReadOnly);
        drop(tmp);
    }

    /// 录制流程开始前手动 pause 时钟 + 起搏器任务（setup 走真实时钟）。
    /// 起搏器把 auto-advance 的推进粒度钳制在 1ms：无起搏时 pending 的远期定时器
    /// （sqlx 30s acquire 超时）会在真实 IO 完成前被瞬间穿透（PoolTimedOut）。
    #[tokio::test]
    async fn test_record_full_flow_starts_background_download() {
        let ch = std_channel("54321");
        let (tmp, reg) = registry(ch.clone(), Arc::new(MockJmcClient::ok("S"))).await;
        tokio::time::pause();
        let pacer = spawn_auto_advance_pacer();
        let out = def(&reg, "jfr_record")
            .handler
            .execute(
                serde_json::json!({"environment": "prod", "pid": "1234", "duration_secs": 10, "timeout_secs": 30}),
                &ctx(),
            )
            .await;
        pacer.abort();
        assert!(out.success, "out: {}", out.data);
        let tid = out.data["transfer_id"].as_str().unwrap();
        assert!(!tid.is_empty());
        assert!(out.data["local_path"].as_str().unwrap().ends_with(".jfr"));
        assert_eq!(out.data["remote_size"], 54321);
        // 命令序列：JFR.start → 若干 stat 轮询
        let calls = ch.calls.lock().await;
        assert!(calls[0].contains("JFR.start"));
        assert!(calls[0].contains("duration=10s"));
        assert!(calls[0].contains("settings=profile"));
        assert!(calls[0].contains("filename=/tmp/friday-tools/recording-1234-"));
        assert!(calls.iter().skip(1).all(|c| c.starts_with("stat -c %s")));
        assert!(calls.len() >= 2);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_record_start_failure_passthrough_with_jdk8_hint() {
        let ch = Arc::new(JfrChannel { start_exit: 1, stat_size: "0", calls: TokioMutex::new(Vec::new()) });
        let (tmp, reg) = registry(ch, Arc::new(MockJmcClient::ok("S"))).await;
        let out = def(&reg, "jfr_record")
            .handler
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "record_failed");
        assert!(
            out.data["message"].as_str().unwrap().contains("arthas_profiler"),
            "JDK 8 fallback hint required"
        );
        drop(tmp);
    }

    /// 录制流程开始前手动 pause 时钟 + 起搏器（同上）
    #[tokio::test]
    async fn test_record_file_never_materializes() {
        let (tmp, reg) = registry(std_channel("0"), Arc::new(MockJmcClient::ok("S"))).await;
        tokio::time::pause();
        let pacer = spawn_auto_advance_pacer();
        let out = def(&reg, "jfr_record")
            .handler
            .execute(
                serde_json::json!({"environment": "prod", "pid": "1234", "duration_secs": 10, "timeout_secs": 30}),
                &ctx(),
            )
            .await;
        pacer.abort();
        assert!(!out.success, "out: {}", out.data);
        assert_eq!(out.data["error"], "record_not_found", "out: {}", out.data);
        assert!(out.data["message"].as_str().unwrap().contains("friday-tools"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_record_invalid_args() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        for args in [
            serde_json::json!({"environment": "prod"}),
            serde_json::json!({"pid": "1234"}),
            serde_json::json!({"environment": "prod", "pid": "1234", "duration_secs": 5}),
            serde_json::json!({"environment": "prod", "pid": "1234", "settings": "boot"}),
            serde_json::json!({"environment": "prod", "pid": "1; rm -rf /"}),
        ] {
            let out = def(&reg, "jfr_record").handler.execute(args, &ctx()).await;
            assert!(!out.success, "args should be rejected");
            assert_eq!(out.data["error"], "invalid_args");
        }
        drop(tmp);
    }

    #[tokio::test]
    async fn test_record_environment_not_found() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        let out = def(&reg, "jfr_record")
            .handler
            .execute(serde_json::json!({"environment": "nope", "pid": "1234"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "environment_not_found");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_proxy_routes_to_upstream_with_path_and_sync() {
        let mock = Arc::new(MockJmcClient::ok("OVERVIEW"));
        let (tmp, reg) = registry(std_channel("1"), mock.clone()).await;
        let p = jfr_file(tmp.path());
        let out = def(&reg, "jfr_overview")
            .handler
            .execute(
                serde_json::json!({"local_path": p.to_string_lossy(), "args": {"start_time": "2026-09-03T10:00:00Z"}}),
                &ctx(),
            )
            .await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["tool"], "jfrOverview");
        let calls = mock.calls.lock().await;
        let (name, args) = calls.last().unwrap();
        assert_eq!(name, "jfrOverview");
        assert_eq!(args["jfr_file_path"].as_str().unwrap(), p.to_string_lossy());
        assert_eq!(args["start_time"], "2026-09-03T10:00:00Z");
        assert_eq!(args["async"], false);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_proxy_missing_params_and_file() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        // 缺 local_path
        let out = def(&reg, "jfr_hot_methods").handler.execute(serde_json::json!({}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_args");
        // 文件不存在
        let out = def(&reg, "jfr_hot_methods")
            .handler
            .execute(serde_json::json!({"local_path": "C:/definitely/nope.jfr"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_path");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_compare_maps_two_paths() {
        let mock = Arc::new(MockJmcClient::ok("DIFF"));
        let (tmp, reg) = registry(std_channel("1"), mock.clone()).await;
        let base = jfr_file(tmp.path());
        let target = {
            let p = tmp.path().join("b.jfr");
            std::fs::write(&p, "fake").unwrap();
            p
        };
        let out = def(&reg, "jfr_compare")
            .handler
            .execute(
                serde_json::json!({
                    "baseline_local_path": base.to_string_lossy(),
                    "target_local_path": target.to_string_lossy()
                }),
                &ctx(),
            )
            .await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["tool"], "compareRecordings");
        let calls = mock.calls.lock().await;
        let (name, args) = calls.last().unwrap();
        assert_eq!(name, "compareRecordings");
        assert_eq!(args["baseline_jfr_path"].as_str().unwrap(), base.to_string_lossy());
        assert_eq!(args["target_jfr_path"].as_str().unwrap(), target.to_string_lossy());
        assert_eq!(args["async"], false);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_compare_requires_both_paths() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        let p = jfr_file(tmp.path());
        let out = def(&reg, "jfr_compare")
            .handler
            .execute(serde_json::json!({"baseline_local_path": p.to_string_lossy()}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_args");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_jmc_unavailable_error_code() {
        let mock = Arc::new(MockJmcClient::with_fn(|_name, _args| async {
            Err("transport closed".to_string())
        }));
        let (tmp, reg) = registry(std_channel("1"), mock).await;
        let p = jfr_file(tmp.path());
        let out = def(&reg, "jfr_overview")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy()}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "jmc_unavailable");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_upstream_error_passthrough_and_truncation() {
        let big = format!("JMC boom\n{}", "x".repeat(70 * 1024));
        let mock = Arc::new(MockJmcClient::with_fn(move |_name, _args| {
            let big = big.clone();
            async move { Ok(crate::analyzer::client::CallOutcome { text: big, is_error: true }) }
        }));
        let (tmp, reg) = registry(std_channel("1"), mock).await;
        let p = jfr_file(tmp.path());
        let out = def(&reg, "jfr_rules")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy()}), &ctx())
            .await;
        assert!(!out.success);
        // 业务错误透传：无 error code，result 携带上游文本 + upstream_is_error
        assert_eq!(out.data["error"], serde_json::Value::Null);
        assert_eq!(out.data["upstream_is_error"], true);
        assert!(out.data["result"].as_str().unwrap().contains("JMC boom"));
        assert_eq!(out.data["truncated"], true);
        assert!(out.data["result"].as_str().unwrap().contains("[truncated"));
        let full = out.data["full_output_path"].as_str().unwrap();
        assert!(std::fs::metadata(full).map(|m| m.len() as usize > 70 * 1024).unwrap_or(false));
        drop(tmp);
    }
}
