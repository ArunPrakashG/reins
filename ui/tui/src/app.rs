use reins_core::Session;

/// Which field of the inline hire prompt is currently accepting keystrokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    HarnessId,
    Role,
    /// The optional opening brief handed to the new team member. Enter on an empty
    /// buffer hires without one.
    Brief,
}

/// The fields collected by the inline hire prompt, returned once it completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HireInput {
    pub harness_id: String,
    pub role: String,
    pub brief: String,
}

pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
    /// `Some(_)` while the three-step inline hire prompt is active; `None` otherwise.
    pub input_mode: Option<InputMode>,
    pub input_harness_id: String,
    pub input_role: String,
    pub input_brief: String,
    /// Most recently fetched raw tmux pane text for the selected session.
    pub pane_content: String,
    /// Last error/notice to show in the status line. The TUI holds the terminal in
    /// raw mode on the alternate screen, so anything printed to stderr would corrupt
    /// the rendered frame — in-loop messages go here and are drawn by `ui::draw`.
    pub status_message: Option<String>,
}

impl App {
    pub fn new() -> Self {
        Self {
            sessions: vec![],
            selected: 0,
            input_mode: None,
            input_harness_id: String::new(),
            input_role: String::new(),
            input_brief: String::new(),
            pane_content: String::new(),
            status_message: None,
        }
    }

    /// Records a message for the status line (replacing any previous one).
    pub fn set_status_message(&mut self, message: impl Into<String>) {
        self.status_message = Some(message.into());
    }

    /// Clears the status line back to the default key hints.
    pub fn clear_status_message(&mut self) {
        self.status_message = None;
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

    /// Begins the three-step inline hire prompt, starting on the harness id field.
    pub fn start_hire_input(&mut self) {
        self.input_mode = Some(InputMode::HarnessId);
        self.clear_input_fields();
    }

    /// Abandons the inline hire prompt without sending anything.
    pub fn cancel_input(&mut self) {
        self.input_mode = None;
        self.clear_input_fields();
    }

    fn clear_input_fields(&mut self) {
        self.input_harness_id.clear();
        self.input_role.clear();
        self.input_brief.clear();
    }

    /// Appends a character to whichever field of the hire prompt is active. No-op if
    /// the prompt isn't active.
    pub fn push_char(&mut self, c: char) {
        match self.input_mode {
            Some(InputMode::HarnessId) => self.input_harness_id.push(c),
            Some(InputMode::Role) => self.input_role.push(c),
            Some(InputMode::Brief) => self.input_brief.push(c),
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
            Some(InputMode::Brief) => {
                self.input_brief.pop();
            }
            None => {}
        }
    }

    /// Advances the hire prompt on Enter: harness id → role → brief. Returns `None`
    /// while more fields remain. Enter on the brief field (empty or not — the brief is
    /// optional) finishes the prompt, resetting `input_mode` to `None` and returning the
    /// collected [`HireInput`]. If the prompt wasn't active, returns `None` and does
    /// nothing.
    pub fn advance_input(&mut self) -> Option<HireInput> {
        match self.input_mode {
            Some(InputMode::HarnessId) => {
                self.input_mode = Some(InputMode::Role);
                None
            }
            Some(InputMode::Role) => {
                self.input_mode = Some(InputMode::Brief);
                None
            }
            Some(InputMode::Brief) => {
                let collected = HireInput {
                    harness_id: self.input_harness_id.clone(),
                    role: self.input_role.clone(),
                    brief: self.input_brief.clone(),
                };
                self.input_mode = None;
                self.clear_input_fields();
                Some(collected)
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
    fn hire_input_collects_three_fields_and_resets() {
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

        // ...and so does Enter on the role field: the brief comes last.
        assert_eq!(app.advance_input(), None);
        assert_eq!(app.input_mode, Some(InputMode::Brief));

        app.push_char('h');
        app.push_char('i');
        assert_eq!(app.input_brief, "hi");

        let result = app.advance_input();
        assert_eq!(
            result,
            Some(HireInput {
                harness_id: "cc".into(),
                role: "rc".into(),
                brief: "hi".into(),
            })
        );
        assert_eq!(app.input_mode, None);
        assert_eq!(app.input_harness_id, "");
        assert_eq!(app.input_role, "");
        assert_eq!(app.input_brief, "");
    }

    #[test]
    fn brief_is_optional_and_completes_the_prompt_when_left_empty() {
        let mut app = App::new();
        app.start_hire_input();
        app.push_char('c');
        app.advance_input();
        app.advance_input();
        assert_eq!(app.input_mode, Some(InputMode::Brief));

        let result = app.advance_input().expect("empty brief still completes the prompt");
        assert_eq!(result.brief, "");
        assert_eq!(result.harness_id, "c");
        assert_eq!(app.input_mode, None);
    }

    #[test]
    fn status_message_round_trips() {
        let mut app = App::new();
        assert_eq!(app.status_message, None);
        app.set_status_message("hire failed: nope");
        assert_eq!(app.status_message.as_deref(), Some("hire failed: nope"));
        app.clear_status_message();
        assert_eq!(app.status_message, None);
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
