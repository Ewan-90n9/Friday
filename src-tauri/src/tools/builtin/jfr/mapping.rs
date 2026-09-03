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
    pub fn upstream_name(&self) -> &'static str {
        match self {
            JfrProxyKind::Overview => "jfr_overview",
            JfrProxyKind::Rules => "jfr_rules",
            JfrProxyKind::QuickAnalysis => "smart_quick_analysis",
            JfrProxyKind::GcDetail => "gc_detail",
            JfrProxyKind::MemoryLeaks => "memory_leaks",
            JfrProxyKind::PredictiveLeak => "smart_predictive_leak_analysis",
            JfrProxyKind::AllocationHotspots => "allocation_hotspots",
            JfrProxyKind::HotMethods => "hot_methods",
            JfrProxyKind::ThreadCpu => "thread_cpu",
            JfrProxyKind::CpuFlame => "cpu_flame",
            JfrProxyKind::ThreadContention => "thread_contention",
            JfrProxyKind::DeadlockDetection => "deadlock_detection",
            JfrProxyKind::IoHotspots => "io_hotspots",
            JfrProxyKind::Exceptions => "exception_analysis",
            JfrProxyKind::Errors => "error_analysis",
            JfrProxyKind::Safepoints => "safepoint_analysis",
            JfrProxyKind::VirtualThreads => "virtual_threads",
            JfrProxyKind::StackTraceSearch => "smart_stack_trace_search",
            JfrProxyKind::Correlate => "smart_correlate",
            JfrProxyKind::RequestWaterfall => "smart_request_waterfall",
        }
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

/// 代理工具：local_path → jfr_file_path + args 透传合并；async 强制 false
/// （禁用上游后台任务模式，靠 Friday 超时分层，spec §3.2）。
pub fn build_proxy(kind: JfrProxyKind, local_path: &str, extra: Option<&Value>) -> (String, Value) {
    let mut map = serde_json::Map::new();
    map.insert("jfr_file_path".to_string(), json!(local_path));
    if let Some(Value::Object(extra)) = extra {
        for (k, v) in extra {
            map.insert(k.clone(), v.clone());
        }
    }
    // 最后强制覆盖：即使调用方透传了 async:true 也压回 false
    map.insert("async".to_string(), json!(false));
    (kind.upstream_name().to_string(), Value::Object(map))
}

/// A/B 对比：双路径映射 + args 透传合并；async 强制 false
pub fn build_compare(baseline: &str, target: &str, extra: Option<&Value>) -> (String, Value) {
    let mut map = serde_json::Map::new();
    map.insert("baseline_jfr_path".to_string(), json!(baseline));
    map.insert("target_jfr_path".to_string(), json!(target));
    if let Some(Value::Object(extra)) = extra {
        for (k, v) in extra {
            map.insert(k.clone(), v.clone());
        }
    }
    map.insert("async".to_string(), json!(false));
    ("smart_compare_recordings".to_string(), Value::Object(map))
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
        assert_eq!(name, "hot_methods");
        assert_eq!(args["jfr_file_path"], r"C:\artifacts\a.jfr");
        assert_eq!(args["top_n"], 5);
        assert_eq!(args["async"], false, "async must be forced false even if caller passes true");
    }

    #[test]
    fn test_build_proxy_without_extra_args() {
        let (name, args) = build_proxy(JfrProxyKind::QuickAnalysis, "/tmp/a.jfr", None);
        assert_eq!(name, "smart_quick_analysis");
        assert_eq!(args["jfr_file_path"], "/tmp/a.jfr");
        assert_eq!(args["async"], false);
        assert_eq!(args.as_object().unwrap().len(), 2);
    }

    #[test]
    fn test_build_compare_two_paths() {
        let (name, args) =
            build_compare("/tmp/base.jfr", "/tmp/target.jfr", Some(&json!({"async": true})));
        assert_eq!(name, "smart_compare_recordings");
        assert_eq!(args["baseline_jfr_path"], "/tmp/base.jfr");
        assert_eq!(args["target_jfr_path"], "/tmp/target.jfr");
        assert_eq!(args["async"], false);
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
