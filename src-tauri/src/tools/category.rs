use serde::{Deserialize, Serialize};

/// 工具分类。声明顺序即面板分组展示顺序（environment → k8s → jvm → heap → jfr → arthas → code → file_transfer → builtin）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Environment,
    K8s,
    Jvm,
    Heap,
    Jfr,
    Arthas,
    Code,
    FileTransfer,
    Builtin,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// serde 序列化名是前端 ToolCategory 联合类型的 IPC 契约，逐变体锁定
    #[test]
    fn test_serde_names_match_frontend_union() {
        let names = [
            (ToolCategory::Environment, "environment"),
            (ToolCategory::K8s, "k8s"),
            (ToolCategory::Jvm, "jvm"),
            (ToolCategory::Heap, "heap"),
            (ToolCategory::Jfr, "jfr"),
            (ToolCategory::Arthas, "arthas"),
            (ToolCategory::Code, "code"),
            (ToolCategory::FileTransfer, "file_transfer"),
            (ToolCategory::Builtin, "builtin"),
        ];
        for (variant, expected) in names {
            assert_eq!(serde_json::to_string(&variant).unwrap(), format!("\"{expected}\""));
        }
    }

    /// 声明序 = 面板分组展示序：K8s 紧跟 Environment
    #[test]
    fn test_declaration_order_k8s_after_environment() {
        assert!(ToolCategory::Environment < ToolCategory::K8s);
        assert!(ToolCategory::K8s < ToolCategory::Jvm);
        assert!(ToolCategory::Arthas < ToolCategory::Code);
        assert!(ToolCategory::Code < ToolCategory::FileTransfer);
    }
}
