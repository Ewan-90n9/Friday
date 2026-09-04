use serde_json::{json, Value};

/// 代理型分析工具的 Friday → 上游映射（Compare 单独走 build_compare）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JfrProxyKind {
    Overview,
    Rules,
    QuickAnalysis,
    GcDetail,
    MemoryLeaks,
    PredictiveLeak,
    AllocationHotspots,
    HotMethods,
    ThreadCpu,
    CpuFlame,
    ThreadContention,
    DeadlockDetection,
    IoHotspots,
    Exceptions,
    Errors,
    Safepoints,
    VirtualThreads,
    StackTraceSearch,
    Correlate,
    RequestWaterfall,
}

impl JfrProxyKind {
    /// 上游实际注册名是 lowerCamelCase（Quarkus MCP 从 Java 方法名派生，与上游
    /// README 文档的 snake_case 不符，实测 tools/list 得出；参数名则保持 snake_case）。
    /// 注意两个特例：VirtualThreads → virtualThreadTool、compare → compareRecordings。
    pub fn upstream_name(&self) -> &'static str {
        match self {
            JfrProxyKind::Overview => "jfrOverview",
            JfrProxyKind::Rules => "jfrRules",
            JfrProxyKind::QuickAnalysis => "smartQuickAnalysis",
            JfrProxyKind::GcDetail => "gcDetail",
            JfrProxyKind::MemoryLeaks => "memoryLeaks",
            JfrProxyKind::PredictiveLeak => "smartPredictiveLeakAnalysis",
            JfrProxyKind::AllocationHotspots => "allocationHotspots",
            JfrProxyKind::HotMethods => "hotMethods",
            JfrProxyKind::ThreadCpu => "threadCpu",
            JfrProxyKind::CpuFlame => "cpuFlame",
            JfrProxyKind::ThreadContention => "threadContention",
            JfrProxyKind::DeadlockDetection => "deadlockDetection",
            JfrProxyKind::IoHotspots => "ioHotspots",
            JfrProxyKind::Exceptions => "exceptionAnalysis",
            JfrProxyKind::Errors => "errorAnalysis",
            JfrProxyKind::Safepoints => "safepointAnalysis",
            JfrProxyKind::VirtualThreads => "virtualThreadTool",
            JfrProxyKind::StackTraceSearch => "smartStackTraceSearch",
            JfrProxyKind::Correlate => "smartCorrelate",
            JfrProxyKind::RequestWaterfall => "smartRequestWaterfall",
        }
    }

    /// 上游 bug 标记（issue #10）：threadCpu / threadContention / virtualThreadTool
    /// 的 top_n 声明为原始 int 且 required=false（上游 6 处同病，Friday 代理的 3 处），
    /// 缺省或显式 null 时 Quarkus invoker 对 null 拆箱 → NPE → -32603 Internal error。
    /// build_proxy 按上游文档默认值注入 top_n=10 兜底。
    /// ⚠ 上游根治（PR 或构建期补丁）落地后本兜底应移除——
    /// 见 docs/superpowers/specs/2026-09-04-jmc-error-classification-followups.md §3.1。
    pub fn needs_top_n_default(&self) -> bool {
        matches!(
            self,
            JfrProxyKind::ThreadCpu | JfrProxyKind::ThreadContention | JfrProxyKind::VirtualThreads
        )
    }
}

/// jfr_record 参数校验：duration_secs（10..=600，默认 60）+ settings 白名单（默认 profile）。
/// Err(String) → invalid_args。
pub fn validate_record_params(args: &Value) -> Result<(u32, String), String> {
    let duration = match args.get("duration_secs").and_then(|v| v.as_i64()) {
        None => 60,
        Some(n) if (10..=600).contains(&n) => n as u32,
        Some(n) => return Err(format!("duration_secs 必须在 10~600 之间，收到 {n}")),
    };
    let settings = match args.get("settings").and_then(|v| v.as_str()) {
        None | Some("profile") => "profile".to_string(),
        Some("default") => "default".to_string(),
        Some(other) => return Err(format!("settings 非法: {other}（可选 profile / default）")),
    };
    Ok((duration, settings))
}

/// jfr_record 有效总超时：默认 600/上限 1800，但必须容纳 duration + 120s 落盘余量。
pub fn effective_record_timeout(user: Option<i64>, duration_secs: u32) -> u64 {
    let base = match user {
        Some(t) if t > 0 => (t as u64).min(1800),
        _ => 600,
    };
    base.max(duration_secs as u64 + 120).min(1800)
}

/// JFR.start 命令构造（一次性定时录制；name/remote_path 由 handler 生成，纯函数可测）
pub fn jfr_start_command(
    jcmd: &str,
    pid: u32,
    name: &str,
    duration_secs: u32,
    settings: &str,
    remote_path: &str,
) -> String {
    format!("{jcmd} {pid} JFR.start name={name} settings={settings} duration={duration_secs}s filename={remote_path}")
}

/// 代理工具：local_path → jfr_file_path + args 透传合并；路径与 async 为 handler
/// 权威值，合并后强制写入（透传对象不得覆盖）；async 固定 false（禁用上游后台
/// 任务模式，靠 Friday 超时分层，spec §3.2）。
pub fn build_proxy(kind: JfrProxyKind, local_path: &str, extra: Option<&Value>) -> (String, Value) {
    let mut map = serde_json::Map::new();
    if let Some(Value::Object(extra)) = extra {
        for (k, v) in extra {
            map.insert(k.clone(), v.clone());
        }
    }
    // 上游 bug 兜底（issue #10）：受影响工具缺省/显式 null 的 top_n 会被上游拆箱 NPE，
    // 强制注入文档默认值 10；显式传值不覆盖。
    if kind.needs_top_n_default() && map.get("top_n").map(|v| v.is_null()).unwrap_or(true) {
        map.insert("top_n".to_string(), json!(10));
    }
    // 最后强制覆盖：路径由 handler 解析（local_path 是唯一来源）；async 压回 false
    map.insert("jfr_file_path".to_string(), json!(local_path));
    map.insert("async".to_string(), json!(false));
    (kind.upstream_name().to_string(), Value::Object(map))
}

/// A/B 对比：双路径映射 + args 透传合并；路径与 async 合并后强制写入
pub fn build_compare(baseline: &str, target: &str, extra: Option<&Value>) -> (String, Value) {
    let mut map = serde_json::Map::new();
    if let Some(Value::Object(extra)) = extra {
        for (k, v) in extra {
            map.insert(k.clone(), v.clone());
        }
    }
    map.insert("baseline_jfr_path".to_string(), json!(baseline));
    map.insert("target_jfr_path".to_string(), json!(target));
    map.insert("async".to_string(), json!(false));
    ("compareRecordings".to_string(), Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_record_params_defaults() {
        let (d, s) = validate_record_params(&json!({})).unwrap();
        assert_eq!(d, 60);
        assert_eq!(s, "profile");
    }

    #[test]
    fn test_validate_record_params_bounds() {
        assert!(validate_record_params(&json!({"duration_secs": 10})).is_ok());
        assert!(validate_record_params(&json!({"duration_secs": 600})).is_ok());
        assert!(validate_record_params(&json!({"duration_secs": 9})).is_err());
        assert!(validate_record_params(&json!({"duration_secs": 601})).is_err());
        assert!(validate_record_params(&json!({"settings": "default"})).is_ok());
        assert!(validate_record_params(&json!({"settings": "boot"})).is_err());
    }

    #[test]
    fn test_jfr_start_command_shape() {
        let cmd = jfr_start_command(
            "/tmp/jdk/bin/jcmd",
            1234,
            "friday-777",
            60,
            "profile",
            "/tmp/friday-tools/recording-1234-777.jfr",
        );
        assert_eq!(
            cmd,
            "/tmp/jdk/bin/jcmd 1234 JFR.start name=friday-777 settings=profile duration=60s filename=/tmp/friday-tools/recording-1234-777.jfr"
        );
    }

    #[test]
    fn test_effective_record_timeout_matrix() {
        assert_eq!(effective_record_timeout(None, 60), 600);
        assert_eq!(effective_record_timeout(None, 300), 600);
        assert_eq!(effective_record_timeout(None, 600), 720);
        assert_eq!(effective_record_timeout(Some(1000), 60), 1000);
        assert_eq!(effective_record_timeout(Some(9999), 60), 1800);
        assert_eq!(effective_record_timeout(Some(30), 600), 720);
        assert_eq!(effective_record_timeout(Some(0), 60), 600);
        assert_eq!(effective_record_timeout(Some(-5), 60), 600);
    }

    #[test]
    fn test_build_proxy_maps_path_and_forces_sync() {
        let (name, args) = build_proxy(
            JfrProxyKind::HotMethods,
            r"C:\artifacts\a.jfr",
            Some(&json!({"top_n": 5, "async": true})),
        );
        assert_eq!(name, "hotMethods");
        assert_eq!(args["jfr_file_path"], r"C:\artifacts\a.jfr");
        assert_eq!(args["top_n"], 5);
        assert_eq!(args["async"], false, "async must be forced false even if caller passes true");

        // 透传对象不得覆盖 handler 权威路径键
        let (_, args) = build_proxy(
            JfrProxyKind::HotMethods,
            r"C:\artifacts\a.jfr",
            Some(&json!({"jfr_file_path": "/stale/hallucinated.jfr"})),
        );
        assert_eq!(args["jfr_file_path"], r"C:\artifacts\a.jfr", "path key must not be overridable");
    }

    #[test]
    fn test_build_proxy_without_extra_args() {
        let (name, args) = build_proxy(JfrProxyKind::QuickAnalysis, "/tmp/a.jfr", None);
        assert_eq!(name, "smartQuickAnalysis");
        assert_eq!(args["jfr_file_path"], "/tmp/a.jfr");
        assert_eq!(args["async"], false);
        assert_eq!(args.as_object().unwrap().len(), 2);
    }

    /// issue #10 回归：受影响工具（threadCpu 等）缺省 top_n 时必须注入默认值 10，
    /// 显式传值不覆盖，显式 null 视同缺省（上游对 null 同样拆箱 NPE）。
    #[test]
    fn test_build_proxy_injects_top_n_default_for_affected_kinds() {
        // 缺省 → 注入 10
        let (name, args) = build_proxy(JfrProxyKind::ThreadCpu, "/tmp/a.jfr", None);
        assert_eq!(name, "threadCpu");
        assert_eq!(args["top_n"], 10);
        // 显式传值 → 不覆盖
        let (_, args) = build_proxy(
            JfrProxyKind::ThreadCpu,
            "/tmp/a.jfr",
            Some(&json!({"top_n": 5})),
        );
        assert_eq!(args["top_n"], 5);
        // 显式 null → 视同缺省，替换为 10
        let (_, args) = build_proxy(
            JfrProxyKind::ThreadCpu,
            "/tmp/a.jfr",
            Some(&json!({"top_n": null})),
        );
        assert_eq!(args["top_n"], 10);
        // 其余两个受影响 kind 同样注入
        let (_, args) = build_proxy(JfrProxyKind::ThreadContention, "/tmp/a.jfr", None);
        assert_eq!(args["top_n"], 10);
        let (_, args) = build_proxy(JfrProxyKind::VirtualThreads, "/tmp/a.jfr", None);
        assert_eq!(args["top_n"], 10);
        // 不受影响 kind（上游用 Integer 判空）不注入
        let (_, args) = build_proxy(JfrProxyKind::HotMethods, "/tmp/a.jfr", None);
        assert!(args.get("top_n").is_none(), "unaffected kinds must not get top_n injected");
    }

    #[test]
    fn test_build_compare_two_paths() {
        let (name, args) =
            build_compare("/tmp/base.jfr", "/tmp/target.jfr", Some(&json!({"async": true})));
        assert_eq!(name, "compareRecordings");
        assert_eq!(args["baseline_jfr_path"], "/tmp/base.jfr");
        assert_eq!(args["target_jfr_path"], "/tmp/target.jfr");
        assert_eq!(args["async"], false);

        // 透传对象不得覆盖 handler 权威路径键
        let (_, args) = build_compare(
            "/tmp/base.jfr",
            "/tmp/target.jfr",
            Some(&json!({"baseline_jfr_path": "/stale.jfr", "target_jfr_path": "/stale.jfr"})),
        );
        assert_eq!(args["baseline_jfr_path"], "/tmp/base.jfr");
        assert_eq!(args["target_jfr_path"], "/tmp/target.jfr");
    }

    #[test]
    fn test_upstream_name_table_complete() {
        let kinds = [
            JfrProxyKind::Overview,
            JfrProxyKind::Rules,
            JfrProxyKind::QuickAnalysis,
            JfrProxyKind::GcDetail,
            JfrProxyKind::MemoryLeaks,
            JfrProxyKind::PredictiveLeak,
            JfrProxyKind::AllocationHotspots,
            JfrProxyKind::HotMethods,
            JfrProxyKind::ThreadCpu,
            JfrProxyKind::CpuFlame,
            JfrProxyKind::ThreadContention,
            JfrProxyKind::DeadlockDetection,
            JfrProxyKind::IoHotspots,
            JfrProxyKind::Exceptions,
            JfrProxyKind::Errors,
            JfrProxyKind::Safepoints,
            JfrProxyKind::VirtualThreads,
            JfrProxyKind::StackTraceSearch,
            JfrProxyKind::Correlate,
            JfrProxyKind::RequestWaterfall,
        ];
        assert_eq!(kinds.len(), 20);
        let names: Vec<&str> = kinds.iter().map(|k| k.upstream_name()).collect();
        assert!(names.iter().all(|n| !n.is_empty()));
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "upstream names must be unique");
    }
}
