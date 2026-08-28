use reins_core::Session;

pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
}

impl App {
    pub fn new() -> Self {
        Self { sessions: vec![], selected: 0 }
    }

    pub fn select_next(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = (self.selected + 1) % self.sessions.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = (self.selected + self.sessions.len() - 1) % self.sessions.len();
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reins_core::SessionStatus;

    fn session(id: &str) -> Session {
        Session {
            id: id.into(), project_id: "p1".into(), harness_id: "claude-code".into(),
            role: None, tmux_session_name: format!("reins-{id}"), status: SessionStatus::Running,
            log_file_path: None, started_at: 0, ended_at: None,
        }
    }

    #[test]
    fn select_next_wraps_around() {
        let mut app = App::new();
        app.sessions = vec![session("a"), session("b")];
        app.select_next();
        assert_eq!(app.selected, 1);
        app.select_next();
        assert_eq!(app.selected, 0);
    }
}
