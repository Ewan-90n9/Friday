use super::spawn::AgentProcess;
use crate::app::events::EventBus;

pub async fn consume_stream(_child: AgentProcess, _bus: &EventBus, _session_id: &str) {
    todo!()
}
