use crate::exec::channel::ExecChannel;
use crate::tools::builtin::jvm::core::{
    clamp_or, error_output, is_jdk_missing, parse_pid, require_bins, resolve_environment,
    JvmExecCore,
};
use crate::tools::builtin::run_command::artifact_dir_for;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

const DUMP_DEFAULT_TIMEOUT_SECS: u64 = 300;
const DUMP_MAX_TIMEOUT_SECS: u64 = 600;
const DOWNLOAD_DEFAULT_TIMEOUT_SECS: u64 = 1800;
const DOWNLOAD_MAX_TIMEOUT_SECS: u64 = 3600;

pub struct HeapDumpHandler {
    pub core: Arc<JvmExecCore>,
    pub bus: crate::app::events::EventBus,
}

#[async_trait]
impl ToolHandler for HeapDumpHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: environment");
        };
        let Some(pid) = args.get("pid").and_then(|v| parse_pid(v)) else {
            return error_output("invalid_params", "pid 必须是正整数字符串");
        };
        let dump_timeout = clamp_or(
            args.get("timeout_secs").and_then(|v| v.as_i64()),
            DUMP_DEFAULT_TIMEOUT_SECS,
            DUMP_MAX_TIMEOUT_SECS,
        );
        let download_timeout = clamp_or(
            args.get("download_timeout_secs").and_then(|v| v.as_i64()),
            DOWNLOAD_DEFAULT_TIMEOUT_SECS,
            DOWNLOAD_MAX_TIMEOUT_SECS,
        );

        let (env, channel) = match resolve_environment(
            &self.core.db,
            &self.core.exec_pool,
            environment,
        )
        .await
        {
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

        // ① 生成（文件名 Friday 固定构造——不开放自定义，注入面）
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let remote_path = format!("/tmp/friday-tools/heapdump-{pid}-{ts}.hprof");
        let dump_cmd = format!("{jcmd} {pid} GC.heap_dump {remote_path}");

        tracing::info!(session_id = %ctx.session_id, env_id = %env.id, pid, command = %dump_cmd, "heap dump: generating");
        let dump_start = std::time::Instant::now();

        let dump_result = tokio::time::timeout(
            std::time::Duration::from_secs(dump_timeout),
            channel.run(&dump_cmd),
        )
        .await;
        let dump_elapsed_ms = dump_start.elapsed().as_millis() as u64;

        let dump_output = match dump_result {
            Err(_) => {
                tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, timeout_secs = dump_timeout, "heap dump generation timed out, dropping connection");
                {
                    let mut pool = self.core.exec_pool.lock().await;
                    pool.disconnect(&env.id).await;
                }
                return error_output(
                    "timeout_error",
                    &format!("heap dump generation timed out after {dump_timeout}s; ssh connection closed"),
                );
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "heap dump exec failed");
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
                    // dump 命令失败：透传 jcmd 输出
                    tracing::error!(session_id = %ctx.session_id, env_id = %env.id, exit_code = output.exit_code, "heap dump command failed");
                    return ToolOutput {
                        success: false,
                        data: serde_json::json!({
                            "error": "dump_failed",
                            "message": "GC.heap_dump 失败",
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

        // ② 校验：stat 文件存在且大小 > 0
        let stat_cmd = format!("stat -c %s {remote_path}");
        let stat_output = channel.run(&stat_cmd).await;
        let remote_size: u64 = match stat_output {
            Ok(o) if o.exit_code == 0 => o.stdout.trim().parse().unwrap_or(0),
            _ => 0,
        };
        if remote_size == 0 {
            tracing::error!(session_id = %ctx.session_id, env_id = %env.id, remote_path, "heap dump file missing or empty after dump");
            return error_output(
                "dump_failed",
                &format!("dump 文件不存在或为空: {remote_path}（jcmd exit 0 但无产物）"),
            );
        }

        self.emit_progress(&ctx.session_id, &format!("dump 生成完成 ({remote_size} bytes)，开始下载"));

        // ③ 拉回：SFTP → session artifacts
        let session_dir = artifact_dir_for(&self.core.artifacts_dir, &ctx.session_id);
        if let Err(e) = tokio::fs::create_dir_all(&session_dir).await {
            tracing::error!(session_id = %ctx.session_id, error = %e, "failed to create session artifacts dir");
            return error_output(
                "download_failed",
                &format!("创建本地 artifacts 目录失败: {e}"),
            );
        }
        let local_path = session_dir.join(format!("heapdump-{pid}-{ts}.hprof"));
        let download_start = std::time::Instant::now();
        let download_result = tokio::time::timeout(
            std::time::Duration::from_secs(download_timeout),
            channel.download(&remote_path, &local_path),
        )
        .await;
        let download_elapsed_ms = download_start.elapsed().as_millis() as u64;

        match download_result {
            Err(_) => {
                tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, remote_path, timeout_secs = download_timeout, "dump download timed out; remote file kept");
                return ToolOutput {
                    success: false,
                    data: serde_json::json!({
                        "error": "download_failed",
                        "message": format!("dump 下载超时（{download_timeout}s）。远端文件保留: {remote_path}，可手动取回。"),
                        "remote_path": remote_path,
                        "remote_size": remote_size,
                        "dump_elapsed_ms": dump_elapsed_ms,
                    }),
                    raw_stdout: None,
                };
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, remote_path, error = %e, "dump download failed; remote file kept");
                return ToolOutput {
                    success: false,
                    data: serde_json::json!({
                        "error": "download_failed",
                        "message": format!("dump 下载失败: {e}。远端文件保留: {remote_path}，可手动取回。"),
                        "remote_path": remote_path,
                        "remote_size": remote_size,
                        "dump_elapsed_ms": dump_elapsed_ms,
                    }),
                    raw_stdout: None,
                };
            }
            Ok(Ok(())) => {}
        }

        // 下载成功 → 清理远端（删 Friday 自己构造路径的文件；失败仅告警不影响结果）
        let cleanup = channel.run(&format!("rm -f {remote_path}")).await;
        if let Err(e) = &cleanup {
            tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "failed to cleanup remote dump file");
        }
        let remote_cleaned = matches!(&cleanup, Ok(o) if o.exit_code == 0);

        self.emit_progress(&ctx.session_id, "dump 下载完成，远端已清理");

        tracing::info!(
            session_id = %ctx.session_id, env_id = %env.id, pid,
            local_path = %local_path.display(), remote_size,
            dump_elapsed_ms, download_elapsed_ms, remote_cleaned,
            "heap dump complete"
        );

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "local_path": local_path.to_string_lossy(),
                "remote_path": remote_path,
                "remote_size": remote_size,
                "dump_elapsed_ms": dump_elapsed_ms,
                "download_elapsed_ms": download_elapsed_ms,
                "remote_cleaned": remote_cleaned,
                "note": "dump 已拉回本地，可交给用户用 MAT 等工具分析。请把 local_path 告知用户。",
            }),
            raw_stdout: Some(dump_output.stdout),
        }
    }
}

impl HeapDumpHandler {
    fn emit_progress(&self, session_id: &str, detail: &str) {
        self.bus.emit(
            session_id,
            crate::app::events::AppEvent::ProvisionProgress {
                session_id: session_id.to_string(),
                tool: "jvm_heap_dump".to_string(),
                stage: "download".to_string(),
                detail: detail.to_string(),
            },
        );
    }
}

pub fn jvm_heap_dump_tool_def(
    core: Arc<JvmExecCore>,
    bus: crate::app::events::EventBus,
) -> ToolDef {
    ToolDef {
        name: "jvm_heap_dump".to_string(),
        description: "对目标 JVM 生成堆转储并自动拉回本地（jcmd GC.heap_dump）。⚠ 高风险：触发 Full GC（STW），大堆可能停顿数十秒；dump 文件可达 GB 级。产物保存在本机会话 artifacts 目录（返回 local_path），请告知用户路径以便用 MAT 等工具分析。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                "pid": { "type": "string", "description": "目标 Java 进程 PID（list_java_processes 返回）" },
                "timeout_secs": { "type": "number", "description": "dump 生成超时秒数，默认 300，上限 600" },
                "download_timeout_secs": { "type": "number", "description": "dump 下载超时秒数，默认 1800，上限 3600（GB 级传输）" }
            },
            "required": ["environment", "pid"]
        }),
        risk_level: RiskLevel::High,
        needs_channel: false,
        handler: Arc::new(HeapDumpHandler { core, bus }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::tools::builtin::jvm::jdk_cache::JdkLayout;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use tokio::sync::Mutex as TokioMutex;

    /// 可编程 mock：按命令内容路由（dump/stat/rm/download）
    struct DumpChannel {
        dump_exit: i32,
        stat_size: &'static str,
        download_ok: bool,
        calls: TokioMutex<Vec<String>>,
    }

    #[async_trait]
    impl ExecChannel for DumpChannel {
        async fn run(
            &self,
            cmd: &str,
        ) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.lock().await.push(cmd.to_string());
            if cmd.contains("GC.heap_dump") {
                return Ok(ExecOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: self.dump_exit,
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
        async fn download(
            &self,
            _remote: &str,
            local: &std::path::Path,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            if !self.download_ok {
                return Err("sftp read error".into());
            }
            std::fs::write(local, b"dump-bytes").map_err(|e| e.to_string())?;
            Ok(())
        }
    }

    async fn setup(channel: Arc<dyn ExecChannel>) -> (tempfile::TempDir, Arc<JvmExecCore>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(
            &db,
            "prod",
            "10.0.0.1",
            22,
            "root",
            "password",
            None,
            None,
        )
        .await
        .unwrap();
        let env_id = crate::app::environments::find_by_name(&db, "prod")
            .await
            .unwrap()
            .unwrap()
            .id;
        let exec_pool =
            Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        exec_pool.lock().await.insert_channel(env_id.clone(), channel).await;
        let mut bins = HashMap::new();
        bins.insert("jcmd".to_string(), "/tmp/jdk/bin/jcmd".to_string());
        let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
        jdk_cache
            .set(
                &env_id,
                JdkLayout { tool_home: "/tmp/jdk".into(), bins },
            )
            .await;
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let core = Arc::new(JvmExecCore { db, exec_pool, jdk_cache, artifacts_dir: artifacts });
        (tmp, core)
    }

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            channel: None,
        }
    }

    fn handler(core: Arc<JvmExecCore>) -> HeapDumpHandler {
        HeapDumpHandler { core, bus: crate::app::events::EventBus::disabled() }
    }

    #[tokio::test]
    async fn test_full_flow_success() {
        let ch = Arc::new(DumpChannel {
            dump_exit: 0,
            stat_size: "12345",
            download_ok: true,
            calls: TokioMutex::new(Vec::new()),
        });
        let (tmp, core) = setup(ch.clone()).await;
        let out = handler(core)
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(out.success, "out: {}", out.data);
        let local = out.data["local_path"].as_str().unwrap();
        assert!(local.ends_with(".hprof"));
        assert!(std::path::Path::new(local).exists(), "local dump must exist");
        assert_eq!(out.data["remote_size"], 12345);
        assert_eq!(out.data["remote_cleaned"], true);
        // 调用序列：dump → stat → rm（download 走 trait 不进 calls）
        let calls = ch.calls.lock().await;
        assert!(calls[0].contains("GC.heap_dump"));
        assert!(calls[1].starts_with("stat -c %s"));
        assert!(calls[2].starts_with("rm -f"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_dump_cmd_failure_passthrough() {
        let ch = Arc::new(DumpChannel {
            dump_exit: 1,
            stat_size: "0",
            download_ok: true,
            calls: TokioMutex::new(Vec::new()),
        });
        let (tmp, core) = setup(ch).await;
        let out = handler(core)
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_failed");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_stat_empty_fails() {
        let ch = Arc::new(DumpChannel {
            dump_exit: 0,
            stat_size: "0",
            download_ok: true,
            calls: TokioMutex::new(Vec::new()),
        });
        let (tmp, core) = setup(ch).await;
        let out = handler(core)
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_failed");
        assert!(out.data["message"].as_str().unwrap().contains("不存在或为空"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_download_failure_keeps_remote_file() {
        let ch = Arc::new(DumpChannel {
            dump_exit: 0,
            stat_size: "12345",
            download_ok: false,
            calls: TokioMutex::new(Vec::new()),
        });
        let (tmp, core) = setup(ch).await;
        let out = handler(core)
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "download_failed");
        assert!(out.data["message"].as_str().unwrap().contains("远端文件保留"));
        assert!(out.data["remote_path"].as_str().unwrap().ends_with(".hprof"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_pid_injection_rejected() {
        let ch = Arc::new(DumpChannel {
            dump_exit: 0,
            stat_size: "1",
            download_ok: true,
            calls: TokioMutex::new(Vec::new()),
        });
        let (tmp, core) = setup(ch).await;
        let out = handler(core)
            .execute(
                serde_json::json!({"environment": "prod", "pid": "1; rm -rf /"}),
                &ctx(),
            )
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_jdk_not_provisioned() {
        let ch = Arc::new(DumpChannel {
            dump_exit: 0,
            stat_size: "1",
            download_ok: true,
            calls: TokioMutex::new(Vec::new()),
        });
        let (tmp, core) = setup(ch).await;
        let env_id = crate::app::environments::find_by_name(&core.db, "prod")
            .await
            .unwrap()
            .unwrap()
            .id;
        core.jdk_cache.clear(&env_id).await;
        let out = handler(core)
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "jdk_not_provisioned");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let ch = Arc::new(DumpChannel {
            dump_exit: 0,
            stat_size: "1",
            download_ok: true,
            calls: TokioMutex::new(Vec::new()),
        });
        let (tmp, core) = setup(ch).await;
        let def = jvm_heap_dump_tool_def(core, crate::app::events::EventBus::disabled());
        assert_eq!(def.name, "jvm_heap_dump");
        assert_eq!(def.risk_level, RiskLevel::High);
        assert!(!def.needs_channel);
        drop(tmp);
    }
}
