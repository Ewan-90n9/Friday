pub mod client;
pub mod java;
// Task 8（lib.rs 装配）前无非测试调用方（heap 工具已接线但未注册），避免 dead_code 告警
#[allow(dead_code)]
pub mod manager;
pub mod session;

// Task 8（lib.rs 装配）前无外部调用方经此再导出引用，避免 unused_imports 告警
#[allow(unused_imports)]
pub use manager::{
    download_complete_hook, normalize_dump_path, HeapAnalyzerManager, ManagerConfig, ManagerError,
};
