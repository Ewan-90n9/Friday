pub mod core;
pub mod heap_dump;
pub mod jdk_cache;
pub mod processes;
pub mod simple;

use crate::app::events::EventBus;
use std::sync::Arc;

/// 注册全部 JVM 工具到 registry（lib.rs 调用）
pub fn register_all(
    registry: &mut crate::tools::registry::ToolRegistry,
    core: Arc<core::JvmExecCore>,
    bus: EventBus,
    transfer: Arc<crate::transfer::TransferManager>,
) {
    registry.register(processes::list_java_processes_tool_def(core.clone()));
    registry.register(simple::jvm_gc_stats_tool_def(core.clone()));
    registry.register(simple::jvm_thread_dump_tool_def(core.clone()));
    registry.register(simple::jvm_heap_info_tool_def(core.clone()));
    registry.register(simple::jvm_vm_info_tool_def(core.clone()));
    registry.register(simple::jvm_class_histogram_tool_def(core.clone()));
    registry.register(heap_dump::jvm_heap_dump_tool_def(core, bus, transfer));
}
