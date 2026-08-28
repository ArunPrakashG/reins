use reins_core::Session;

/// Which field of the inline hire prompt is currently accepting keystrokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    HarnessId,
    Role,
}

pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
    /// `Some(_)` while the two-step inline hire prompt is active; `None` otherwise.
    pub input_mode: Option<InputMode>,
    pub input_harness_id: String,
    pub input_role: String,
    /// Most recently fetched raw tmux pane text for the selected session.
    pub pane_content: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            sessions: vec![],
            selected: 0,
            input_mode: None,
            input_harness_id: String::new(),
            input_role: String::new(),
            pane_content: String::new(),
        }
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

    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.selected)
    }

    /// Begins the two-step inline hire prompt, starting on the harness id field.
    pub fn start_hire_input(&mut self) {
        self.input_mode = Some(InputMode::HarnessId);
        self.input_harness_id.clear();
        self.input_role.clear();
    }

    /// Abandons the inline hire prompt without sending anything.
    pub fn cancel_input(&mut self) {
        self.input_mode = None;
        self.input_harness_id.clear();
        self.input_role.clear();
    }

    /// Appends a character to whichever field of the hire prompt is active. No-op if
    /// the prompt isn't active.
    pub fn push_char(&mut self, c: char) {
        match self.input_mode {
            Some(InputMode::HarnessId) => self.input_harness_id.push(c),
            Some(InputMode::Role) => self.input_role.push(c),
            None => {}
        }
    }

    /// Removes the last character from whichever field of the hire prompt is active.
    /// No-op if the prompt isn't active or the field is already empty.
    pub fn backspace(&mut self) {
        match self.input_mode {
            Some(InputMode::HarnessId) => {
                self.input_harness_id.pop();
            }
            Some(InputMode::Role) => {
                self.input_role.pop();
            }
            None => {}
        }
    }

    /// Advances the hire prompt on Enter. From the harness id field, moves to the role
    /// field and returns `None`. From the role field, finishes the prompt (resetting
    /// `input_mode` to `None`) and returns the collected `(harness_id, role)` pair. If
    /// the prompt wasn't active, returns `None` and does nothing.
    pub fn advance_input(&mut self) -> Option<(String, String)> {
        match self.input_mode {
            Some(InputMode::HarnessId) => {
                self.input_mode = Some(InputMode::Role);
                None
            }
            Some(InputMode::Role) => {
                let harness_id = self.input_harness_id.clone();
                let role = self.input_role.clone();
                self.input_mode = None;
                self.input_harness_id.clear();
                self.input_role.clear();
                Some((harness_id, role))
            }
            None => None,
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

    #[test]
    fn hire_input_collects_two_fields_and_resets() {
        let mut app = App::new();
        assert_eq!(app.input_mode, None);

        app.start_hire_input();
        assert_eq!(app.input_mode, Some(InputMode::HarnessId));

        app.push_char('c');
        app.push_char('c');
        assert_eq!(app.input_harness_id, "cc");

        // Enter on the first field just advances, no result yet.
        assert_eq!(app.advance_input(), None);
        assert_eq!(app.input_mode, Some(InputMode::Role));

        app.push_char('A');
        app.backspace();
        app.push_char('r');
        app.push_char('c');
        assert_eq!(app.input_role, "rc");

        let result = app.advance_input();
        assert_eq!(result, Some(("cc".to_string(), "rc".to_string())));
        assert_eq!(app.input_mode, None);
        assert_eq!(app.input_harness_id, "");
        assert_eq!(app.input_role, "");
    }

    #[test]
    fn cancel_input_clears_state_without_returning_a_result() {
        let mut app = App::new();
        app.start_hire_input();
        app.push_char('x');
        app.cancel_input();
        assert_eq!(app.input_mode, None);
        assert_eq!(app.input_harness_id, "");
    }

    #[test]
    fn push_char_and_backspace_are_noops_when_prompt_inactive() {
        let mut app = App::new();
        app.push_char('x');
        app.backspace();
        assert_eq!(app.input_harness_id, "");
        assert_eq!(app.advance_input(), None);
    }

    #[test]
    fn selected_session_returns_none_when_empty() {
        let app = App::new();
        assert!(app.selected_session().is_none());
    }

    #[test]
    fn selected_session_returns_current_selection() {
        let mut app = App::new();
        app.sessions = vec![session("a"), session("b")];
        app.select_next();
        assert_eq!(app.selected_session().unwrap().id, "b");
    }
}
