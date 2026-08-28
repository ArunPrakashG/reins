use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    Starting,
    Running,
    AwaitingInput,
    Exited,
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project_id: String,
    pub harness_id: String,
    pub role: Option<String>,
    pub tmux_session_name: String,
    pub status: SessionStatus,
    pub log_file_path: Option<PathBuf>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

impl Session {
    /// UI display label: "{role} ({harness_id})", or just "{harness_id}" if no role.
    pub fn display_label(&self) -> String {
        match &self.role {
            Some(role) => format!("{role} ({})", self.harness_id),
            None => self.harness_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(role: Option<&str>) -> Session {
        Session {
            id: "s1".into(),
            project_id: "p1".into(),
            harness_id: "claude-code".into(),
            role: role.map(String::from),
            tmux_session_name: "reins-s1".into(),
            status: SessionStatus::Running,
            log_file_path: None,
            started_at: 0,
            ended_at: None,
        }
    }

    #[test]
    fn display_label_with_role() {
        assert_eq!(sample(Some("Architect")).display_label(), "Architect (claude-code)");
    }

    #[test]
    fn display_label_without_role() {
        assert_eq!(sample(None).display_label(), "claude-code");
    }
}
