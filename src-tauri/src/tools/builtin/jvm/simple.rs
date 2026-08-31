use crate::tools::builtin::jvm::core::{
    clamp_or, error_output, parse_pid, require_bins, resolve_environment, JvmExecCore,
};
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

/// 每工具的超时配置（默认/上限）
pub struct Timeouts {
    pub default_secs: u64,
    pub max_secs: u64,
}

const GC_STATS: Timeouts = Timeouts { default_secs: 30, max_secs: 300 };
const THREAD_DUMP: Timeouts = Timeouts { default_secs: 60, max_secs: 300 };
const HEAP_INFO: Timeouts = Timeouts { default_secs: 60, max_secs: 300 };
const VM_INFO: Timeouts = Timeouts { default_secs: 60, max_secs: 300 };
const CLASS_HISTOGRAM: Timeouts = Timeouts { default_secs: 120, max_secs: 600 };

/// 通用 JVM 命令 handler：bin_key（jstat/jcmd）+ 命令构造器
pub struct JvmSimpleHandler {
    pub core: Arc<JvmExecCore>,
    pub bin_key: &'static str,
    pub timeouts: &'static Timeouts,
    /// 由 (bin_path, args, pid) 构造完整命令；Err → invalid_params
    pub build_command: fn(&str, &serde_json::Value, u32) -> Result<String, String>,
}

#[async_trait]
impl ToolHandler for JvmSimpleHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: environment");
        };
        let Some(pid) = args.get("pid").and_then(|v| parse_pid(v)) else {
            return error_output("invalid_params", "pid 必须是正整数字符串");
        };
        let timeout_secs = clamp_or(
            args.get("timeout_secs").and_then(|v| v.as_i64()),
            self.timeouts.default_secs,
            self.timeouts.max_secs,
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
        let bins = match require_bins(&layout, &[self.bin_key]) {
            Ok(b) => b,
            Err(e) => return error_output("jdk_not_provisioned", &e),
        };
        let bin_path = bins[0].clone();

        let command = match (self.build_command)(&bin_path, &args, pid) {
            Ok(c) => c,
            Err(e) => return error_output("invalid_params", &e),
        };

        tracing::info!(session_id = %ctx.session_id, env_id = %env.id, pid, command, "jvm tool executing");
        self.core
            .exec_jdk_command(
                &ctx.session_id,
                &env.id,
                &channel,
                &bin_path,
                &command,
                timeout_secs,
                "log",
            )
            .await
    }
}

// ── 命令构造器（纯函数，单独可测）──

fn build_gc_stats(bin: &str, args: &serde_json::Value, pid: u32) -> Result<String, String> {
    let mut cmd = format!("{bin} -gcutil {pid}");
    if let Some(interval) = args.get("interval_ms").and_then(|v| v.as_i64()) {
        if interval <= 0 {
            return Err("interval_ms 必须是正整数（毫秒）".into());
        }
        let count = args.get("count").and_then(|v| v.as_i64()).unwrap_or(10);
        if count <= 0 {
            return Err("count 必须是正整数".into());
        }
        cmd.push_str(&format!(" {interval} {count}"));
    }
    Ok(cmd)
}

fn build_thread_dump(bin: &str, _args: &serde_json::Value, pid: u32) -> Result<String, String> {
    Ok(format!("{bin} {pid} Thread.print -l"))
}

fn build_heap_info(bin: &str, _args: &serde_json::Value, pid: u32) -> Result<String, String> {
    Ok(format!("{bin} {pid} GC.heap_info"))
}

fn build_vm_info(bin: &str, args: &serde_json::Value, pid: u32) -> Result<String, String> {
    let info_type =
        args.get("info_type").and_then(|v| v.as_str()).unwrap_or("command_line");
    let sub = match info_type {
        "version" => "VM.version",
        "uptime" => "VM.uptime",
        "command_line" => "VM.command_line",
        "flags" => "VM.flags",
        "system_properties" => "VM.system_properties",
        other => {
            return Err(format!(
                "info_type 非法: {other:?}（可选 version/uptime/command_line/flags/system_properties）"
            ))
        }
    };
    Ok(format!("{bin} {pid} {sub}"))
}

fn build_class_histogram(
    bin: &str,
    args: &serde_json::Value,
    pid: u32,
) -> Result<String, String> {
    let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    if all {
        Ok(format!("{bin} {pid} GC.class_histogram -all"))
    } else {
        Ok(format!("{bin} {pid} GC.class_histogram"))
    }
}

/// 公共 schema：environment + pid + timeout_secs，外加每工具专属属性
fn simple_schema(
    timeouts: &Timeouts,
    extra_props: Vec<(&str, serde_json::Value)>,
) -> serde_json::Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "environment".into(),
        serde_json::json!({ "type": "string", "description": "目标环境名称（list_environments 返回的 name）" }),
    );
    props.insert(
        "pid".into(),
        serde_json::json!({ "type": "string", "description": "目标 JVM 进程 PID（正整数字符串，用 list_processes 获取）" }),
    );
    props.insert(
        "timeout_secs".into(),
        serde_json::json!({
            "type": "number",
            "description": format!("超时秒数，默认 {}，上限 {}", timeouts.default_secs, timeouts.max_secs)
        }),
    );
    for (k, v) in extra_props {
        props.insert((*k).into(), v);
    }
    serde_json::json!({
        "type": "object",
        "properties": props,
        "required": ["environment", "pid"],
    })
}

pub fn jvm_gc_stats_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_gc_stats".to_string(),
        description: "采集目标 JVM 的 GC 统计（jstat -gcutil：各代占用百分比、GC 次数/耗时）。诊断 OOM/GC 频繁/内存泄漏的首选。可传 interval_ms + count 连续采样观察趋势（如 interval_ms=1000, count=5）。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: simple_schema(
            &GC_STATS,
            vec![
                ("interval_ms", serde_json::json!({ "type": "number", "description": "采样间隔（毫秒），正整数；传入后连续采样 count 次观察趋势" })),
                ("count", serde_json::json!({ "type": "number", "description": "采样次数，默认 10，正整数；仅在 interval_ms 存在时生效" })),
            ],
        ),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler {
            core,
            bin_key: "jstat",
            timeouts: &GC_STATS,
            build_command: build_gc_stats,
        }),
    }
}

pub fn jvm_thread_dump_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_thread_dump".to_string(),
        description: "抓取目标 JVM 线程转储（jcmd Thread.print -l，含死锁检测信息）。诊断 CPU 飙高、死锁、线程阻塞。输出较长，可直接读关键段（BLOCKED/死锁/等待）。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: simple_schema(&THREAD_DUMP, vec![]),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler {
            core,
            bin_key: "jcmd",
            timeouts: &THREAD_DUMP,
            build_command: build_thread_dump,
        }),
    }
}

pub fn jvm_heap_info_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_heap_info".to_string(),
        description: "查看目标 JVM 堆概况（jcmd GC.heap_info：各代容量/已用、GC 策略）。OOM 时确认堆配置与实际占用。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: simple_schema(&HEAP_INFO, vec![]),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler {
            core,
            bin_key: "jcmd",
            timeouts: &HEAP_INFO,
            build_command: build_heap_info,
        }),
    }
}

pub fn jvm_vm_info_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_vm_info".to_string(),
        description: "查看目标 JVM 基础信息（jcmd VM.*）：info_type 可选 version/uptime/command_line/flags/system_properties（默认 command_line）。确认 JVM 版本、启动参数、系统属性。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: simple_schema(
            &VM_INFO,
            vec![(
                "info_type",
                serde_json::json!({
                    "type": "string",
                    "enum": ["version", "uptime", "command_line", "flags", "system_properties"],
                    "description": "信息类型，默认 command_line"
                }),
            )],
        ),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler {
            core,
            bin_key: "jcmd",
            timeouts: &VM_INFO,
            build_command: build_vm_info,
        }),
    }
}

pub fn jvm_class_histogram_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_class_histogram".to_string(),
        description: "统计目标 JVM 存活对象直方图（jcmd GC.class_histogram，按类聚合实例数/字节）。定位大对象/内存泄漏（哪个类实例最多）。注意：默认 live 视图会触发一次 Full GC；传 all=true 含死对象不强制 GC。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: simple_schema(
            &CLASS_HISTOGRAM,
            vec![(
                "all",
                serde_json::json!({
                    "type": "boolean",
                    "description": "true 时统计含死对象（-all，不强制 Full GC）；默认 false 仅 live 对象（触发一次 Full GC）"
                }),
            )],
        ),
        risk_level: RiskLevel::Low,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler {
            core,
            bin_key: "jcmd",
            timeouts: &CLASS_HISTOGRAM,
            build_command: build_class_histogram,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::tools::builtin::jvm::jdk_cache::JdkLayout;
    use async_trait::async_trait;
    use std::collections::HashMap;

    /// 不校验命令前缀的简单 channel（jstat/jcmd 通用）
    struct OkChannel;

    #[async_trait]
    impl ExecChannel for OkChannel {
        async fn run(
            &self,
            _cmd: &str,
        ) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput {
                stdout: "S0 S1 E O M YGC FGC".into(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool {
            true
        }
    }

    async fn setup() -> (tempfile::TempDir, Arc<JvmExecCore>, String) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        let env_id = crate::app::env_save::save_environment(
            &db, None, "prod", "10.0.0.1", 22,
            vec![crate::app::env_save::CredentialInput {
                id: None,
                username: "root".to_string(),
                auth_type: "password".to_string(),
                private_key_path: None,
                secret: None,
                is_default: true,
            }],
        ).await.unwrap().environment.id;
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        exec_pool.lock().await.insert_channel(env_id.clone(), Arc::new(OkChannel)).await;
        let mut bins = HashMap::new();
        bins.insert("jstat".to_string(), "/tmp/jdk/bin/jstat".to_string());
        bins.insert("jcmd".to_string(), "/tmp/jdk/bin/jcmd".to_string());
        let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
        jdk_cache.set(&env_id, JdkLayout { tool_home: "/tmp/jdk".into(), bins }).await;
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let core = Arc::new(JvmExecCore { db, exec_pool, jdk_cache, artifacts_dir: artifacts });
        (tmp, core, env_id)
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: "123e4567-e89b-12d3-a456-426614174000".into(), channel: None }
    }

    #[tokio::test]
    async fn test_gc_stats_builds_jstat_command() {
        let (tmp, core, _) = setup().await;
        let handler =
            JvmSimpleHandler { core, bin_key: "jstat", timeouts: &GC_STATS, build_command: build_gc_stats };
        let out = handler
            .execute(
                serde_json::json!({"environment": "prod", "pid": "1234", "interval_ms": 1000, "count": 5}),
                &ctx(),
            )
            .await;
        assert!(out.success, "out: {}", out.data);
        assert!(
            out.data["command"]
                .as_str()
                .unwrap()
                .starts_with("/tmp/jdk/bin/jstat -gcutil 1234 1000 5")
        );
        drop(tmp);
    }

    #[tokio::test]
    async fn test_pid_injection_rejected() {
        let (tmp, core, _) = setup().await;
        let handler =
            JvmSimpleHandler { core, bin_key: "jstat", timeouts: &GC_STATS, build_command: build_gc_stats };
        let out = handler
            .execute(
                serde_json::json!({"environment": "prod", "pid": "1234; rm -rf /"}),
                &ctx(),
            )
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_jdk_not_provisioned_guides_ensure_tool() {
        let (tmp, core, env_id) = setup().await;
        core.jdk_cache.clear(&env_id).await;
        let handler =
            JvmSimpleHandler { core, bin_key: "jstat", timeouts: &GC_STATS, build_command: build_gc_stats };
        let out = handler
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "jdk_not_provisioned");
        assert!(out.data["message"].as_str().unwrap().contains("ensure_tool"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_vm_info_rejects_bad_info_type() {
        let (tmp, core, _) = setup().await;
        let handler =
            JvmSimpleHandler { core, bin_key: "jcmd", timeouts: &VM_INFO, build_command: build_vm_info };
        let out = handler
            .execute(
                serde_json::json!({"environment": "prod", "pid": "1234", "info_type": "evil; rm -rf /"}),
                &ctx(),
            )
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[test]
    fn test_build_vm_info_all_types() {
        for (t, sub) in [
            ("version", "VM.version"),
            ("uptime", "VM.uptime"),
            ("command_line", "VM.command_line"),
            ("flags", "VM.flags"),
            ("system_properties", "VM.system_properties"),
        ] {
            let cmd =
                build_vm_info("/jdk/bin/jcmd", &serde_json::json!({"info_type": t}), 42).unwrap();
            assert_eq!(cmd, format!("/jdk/bin/jcmd 42 {sub}"));
        }
        let cmd = build_vm_info("/jdk/bin/jcmd", &serde_json::json!({}), 42).unwrap();
        assert_eq!(cmd, "/jdk/bin/jcmd 42 VM.command_line");
    }

    #[test]
    fn test_build_gc_stats_validation() {
        assert!(
            build_gc_stats("/b/jstat", &serde_json::json!({"interval_ms": 0}), 1).is_err()
        );
        assert!(
            build_gc_stats("/b/jstat", &serde_json::json!({"interval_ms": 1000, "count": -1}), 1)
                .is_err()
        );
        assert_eq!(
            build_gc_stats("/b/jstat", &serde_json::json!({}), 1).unwrap(),
            "/b/jstat -gcutil 1"
        );
    }

    #[test]
    fn test_build_class_histogram_all_flag() {
        assert_eq!(
            build_class_histogram("/b/jcmd", &serde_json::json!({}), 1).unwrap(),
            "/b/jcmd 1 GC.class_histogram"
        );
        assert_eq!(
            build_class_histogram("/b/jcmd", &serde_json::json!({"all": true}), 1).unwrap(),
            "/b/jcmd 1 GC.class_histogram -all"
        );
    }

    #[tokio::test]
    async fn test_tool_defs_metadata() {
        let (tmp, core, _) = setup().await;
        assert_eq!(jvm_gc_stats_tool_def(core.clone()).risk_level, RiskLevel::ReadOnly);
        assert_eq!(jvm_thread_dump_tool_def(core.clone()).risk_level, RiskLevel::ReadOnly);
        assert_eq!(jvm_heap_info_tool_def(core.clone()).risk_level, RiskLevel::ReadOnly);
        assert_eq!(jvm_vm_info_tool_def(core.clone()).risk_level, RiskLevel::ReadOnly);
        assert_eq!(jvm_class_histogram_tool_def(core.clone()).risk_level, RiskLevel::Low);
        assert_eq!(jvm_class_histogram_tool_def(core).name, "jvm_class_histogram");
        drop(tmp);
    }
}
