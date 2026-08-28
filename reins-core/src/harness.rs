use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessStatus {
    Idle,
    Running,
    AwaitingInput,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessProfile {
    pub id: String,
    pub display_name: String,
    pub strengths: Vec<String>,
    pub constraints: Vec<String>,
    pub notes: String,
}
