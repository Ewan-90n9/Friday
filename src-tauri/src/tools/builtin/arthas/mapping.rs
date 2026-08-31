use serde_json::Value;

/// Friday 工具 kind（Open/Close 之外 25 个代理到 arthas MCP 同名工具）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArthasToolKind {
    Open,
    Close,
    Dashboard,
    Jvm,
    Memory,
    Sysenv,
    Perfcounter,
    Sc,
    Sm,
    Jad,
    Classloader,
    Getstatic,
    Mbean,
    Dump,
    Thread,
    Viewfile,
    Options,
    Watch,
    Trace,
    Stack,
    Monitor,
    Tt,
    Ognl,
    Vmtool,
    Sysprop,
    Vmoption,
    Profiler,
}

/// 上游 arthas MCP 工具名（与 arthas 命令同名）
pub fn upstream_name(kind: ArthasToolKind) -> &'static str {
    match kind {
        ArthasToolKind::Open | ArthasToolKind::Close => "",
        ArthasToolKind::Dashboard => "dashboard",
        ArthasToolKind::Jvm => "jvm",
        ArthasToolKind::Memory => "memory",
        ArthasToolKind::Sysenv => "sysenv",
        ArthasToolKind::Perfcounter => "perfcounter",
        ArthasToolKind::Sc => "sc",
        ArthasToolKind::Sm => "sm",
        ArthasToolKind::Jad => "jad",
        ArthasToolKind::Classloader => "classloader",
        ArthasToolKind::Getstatic => "getstatic",
        ArthasToolKind::Mbean => "mbean",
        ArthasToolKind::Dump => "dump",
        ArthasToolKind::Thread => "thread",
        ArthasToolKind::Viewfile => "viewfile",
        ArthasToolKind::Options => "options",
        ArthasToolKind::Watch => "watch",
        ArthasToolKind::Trace => "trace",
        ArthasToolKind::Stack => "stack",
        ArthasToolKind::Monitor => "monitor",
        ArthasToolKind::Tt => "tt",
        ArthasToolKind::Ognl => "ognl",
        ArthasToolKind::Vmtool => "vmtool",
        ArthasToolKind::Sysprop => "sysprop",
        ArthasToolKind::Vmoption => "vmoption",
        ArthasToolKind::Profiler => "profiler",
    }
}

/// Friday 工具参数 → 上游 arthas MCP 工具参数（args 对象原样透传）。
/// 子操作过滤：thread/vmtool 拒绝 interrupt。
pub fn build_args(kind: ArthasToolKind, args: &Value) -> Result<Value, String> {
    match kind {
        ArthasToolKind::Open | ArthasToolKind::Close => {
            Err("内部错误：open/close 不经 mapping".to_string())
        }
        ArthasToolKind::Thread => {
            let upstream = passthrough(args)?;
            if upstream.get("interrupt").is_some() {
                return Err(
                    "thread 的 interrupt 子操作不被支持（会打断目标线程）；支持查看线程列表/栈/状态"
                        .to_string(),
                );
            }
            Ok(upstream)
        }
        ArthasToolKind::Vmtool => {
            let upstream = passthrough(args)?;
            if upstream.get("action").and_then(|v| v.as_str()) == Some("interrupt") {
                return Err(
                    "vmtool 的 interrupt 子操作不被支持；支持 forceGc / getInstances".to_string()
                );
            }
            Ok(upstream)
        }
        _ => passthrough(args),
    }
}

fn passthrough(args: &Value) -> Result<Value, String> {
    match args.get("args") {
        None | Some(Value::Null) => Ok(serde_json::json!({})),
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        Some(_) => Err("args 必须是对象（arthas 命令参数的字段形式）".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_upstream_names() {
        assert_eq!(upstream_name(ArthasToolKind::Dashboard), "dashboard");
        assert_eq!(upstream_name(ArthasToolKind::Watch), "watch");
        assert_eq!(upstream_name(ArthasToolKind::Vmoption), "vmoption");
        assert_eq!(upstream_name(ArthasToolKind::Profiler), "profiler");
    }

    #[test]
    fn test_build_args_passthrough() {
        let args = build_args(ArthasToolKind::Watch, &json!({"args": {"classPattern": "com.foo.Bar"}})).unwrap();
        assert_eq!(args, json!({"classPattern": "com.foo.Bar"}));
        // 缺省 args → 空对象
        let args = build_args(ArthasToolKind::Dashboard, &json!({})).unwrap();
        assert_eq!(args, json!({}));
    }

    #[test]
    fn test_build_args_rejects_non_object() {
        let err = build_args(ArthasToolKind::Watch, &json!({"args": "watch com.foo.Bar m"}));
        assert!(err.is_err());
    }

    #[test]
    fn test_thread_interrupt_filtered() {
        let err = build_args(ArthasToolKind::Thread, &json!({"args": {"id": 1, "interrupt": true}}));
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("interrupt"));
        // 不带 interrupt 的正常透传
        let ok = build_args(ArthasToolKind::Thread, &json!({"args": {"id": 1}})).unwrap();
        assert_eq!(ok, json!({"id": 1}));
    }

    #[test]
    fn test_vmtool_interrupt_filtered() {
        let err = build_args(ArthasToolKind::Vmtool, &json!({"args": {"action": "interrupt", "threadId": 3}}));
        assert!(err.is_err());
        let ok = build_args(ArthasToolKind::Vmtool, &json!({"args": {"action": "forceGc"}})).unwrap();
        assert_eq!(ok, json!({"action": "forceGc"}));
    }
}
