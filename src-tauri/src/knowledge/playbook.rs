use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playbook {
    pub symptom: String,
    pub steps: Vec<PlaybookStep>,
    pub notes: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaybookStep {
    pub tool: String,
    pub args: serde_json::Value,
    pub interpret: String,
}

pub async fn get_playbook(_symptom: &str) -> Option<Playbook> {
    todo!()
}
