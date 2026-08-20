use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionId(pub String);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub env: String,
    pub service: String,
    pub symptom: String,
    pub status: SessionStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Closed,
}

pub async fn create_session(
    _env: &str,
    _service: &str,
    _symptom: &str,
) -> Result<Session, Box<dyn std::error::Error + Send + Sync>> {
    todo!()
}

pub async fn close_session(_id: &SessionId) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    todo!()
}
