use serde::{Deserialize, Serialize};

/// 工具分类。声明顺序即面板分组展示顺序（environment → jvm → heap → arthas → file_transfer → builtin）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Environment,
    Jvm,
    Heap,
    Arthas,
    FileTransfer,
    Builtin,
}
