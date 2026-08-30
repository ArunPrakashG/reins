use reins_core::{HarnessProfile, Session, SessionStatus};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How long a first quit request (`q`, Ctrl+C, or an external SIGINT/SIGTERM) stays
/// "armed" waiting for a confirming second request before it's forgotten.
pub const QUIT_CONFIRM_WINDOW: Duration = Duration::from_secs(2);

/// Which step of the inline hire prompt is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Picking from [`App::available_harnesses`] with up/down — not free text.
    HarnessId,
    /// Free text, pre-filled with reins' own current directory as a default the user
    /// can just accept (Enter) or overwrite.
    WorkingDirectory,
    Role,
    /// The optional opening brief handed to the new team member. Enter on an empty
    /// buffer hires without one.
    Brief,
}

/// The fields collected by the inline hire prompt, returned once it completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HireInput {
    pub harness_id: String,
    pub working_dir: String,
    pub role: String,
    pub brief: String,
}

pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
    /// `Some(_)` while the three-step inline hire prompt is active; `None` otherwise.
    pub input_mode: Option<InputMode>,
    /// Harnesses available to hire, fetched via `Request::ListHarnesses` right before
    /// the prompt opens. Already availability-filtered by the daemon (`is_available()`,
    /// see `HarnessAdapter`), so every entry here is actually hireable — no dead
    /// options to guess around. Populated by [`Self::start_hire_input`].
    pub available_harnesses: Vec<HarnessProfile>,
    /// Index into `available_harnesses` for the picker's current highlight.
    pub harness_picker_index: usize,
    pub input_harness_id: String,
    pub input_working_dir: String,
    pub input_role: String,
    pub input_brief: String,
    /// Most recently fetched pane content for the selected session, as tmux's
    /// `capture-pane -e` output (color/style escape codes included) — fed into a
    /// `vt100::Parser` by `ui::draw_pane` for styled rendering, not printed raw.
    pub pane_content: String,
    /// The selected session's pane cursor position `(x, y)`, from the same
    /// `GetPaneSnapshot` response as `pane_content`. Only meaningful while
    /// [`Self::focused`] — the cursor isn't drawn otherwise.
    pub pane_cursor: (u16, u16),
    /// `true` while keystrokes are being forwarded into the selected session's pane
    /// instead of driving reins' own roster navigation — see [`Self::enter_focus`].
    focused: bool,
    /// `true` for the one keystroke immediately after the focus-mode prefix chord
    /// (`Ctrl-B`), which that next keystroke is interpreted as a reins command against
    /// (currently only `d` to defocus) rather than forwarded to the pane — the same
    /// prefix-key convention tmux itself uses, so a real keystroke meant for the
    /// harness is never ambiguous with a command meant for reins.
    prefix_pending: bool,
    /// Last error/notice to show in the status line. The TUI holds the terminal in
    /// raw mode on the alternate screen, so anything printed to stderr would corrupt
    /// the rendered frame — in-loop messages go here and are drawn by `ui::draw`.
    pub status_message: Option<String>,
    /// When each currently-`Starting` session was first observed in that status, keyed
    /// by session id. Drives the "hiring" pulse animation's own elapsed time (Task 12) —
    /// populated by [`Self::sync_hire_tracking`] and cleared once a session's status
    /// moves off `Starting` or it drops out of the roster entirely.
    ///
    /// `ui::animate_roster_glyphs` feeds `Instant::elapsed()` off this straight into
    /// `effects::apply_glyph_animation`, which computes the pulse color as a pure
    /// function of elapsed-time-mod-period (see `effects::pulse_alpha`'s doc comment) —
    /// there's no persistent `tachyonfx::Effect` state to keep in sync here, unlike
    /// `effects::play_splash`'s one-shot entrance effect. An earlier version of this
    /// code tried driving a real `tachyonfx` `Effect` (`fx::ping_pong(fx::fade_to_fg(..))`)
    /// for this, which turned out to be broken in two independent ways under code
    /// review — see `effects::pulse_alpha` for the full explanation of why the animation
    /// is computed by hand instead.
    pub hire_started_at: HashMap<String, Instant>,
    /// When this `App` was created. Drives the `Running` status glyph's subtle
    /// continuous pulse, which is keyed off elapsed time since app start rather than
    /// needing its own per-session tracking state (Task 12).
    pub started_at: Instant,
    /// Whether animated status glyphs are enabled (from `config::load().animations`,
    /// Task 6). Cached here rather than re-read from disk on every frame; the caller
    /// sets this once at startup.
    pub animations_enabled: bool,
    /// `Some(when)` while a quit request (`q`, Ctrl+C, or an external SIGINT/SIGTERM)
    /// is "armed" waiting for a confirming second request within
    /// [`QUIT_CONFIRM_WINDOW`]. Set and read via [`Self::request_quit`] and
    /// [`Self::quit_warning_active`] — never assigned directly outside those.
    quit_requested_at: Option<Instant>,
    /// Set when a background version check (see `daemon::updater::background_check`)
    /// finds a newer release. Holds the raw GitHub tag (e.g. `"v0.2.0"`). Shown in the
    /// status line at lowest priority — the quit-warning and focus-mode indicators
    /// both still take over the line ahead of this.
    pub update_available: Option<String>,
}

impl App {
    pub fn new() -> Self {
        Self {
            sessions: vec![],
            selected: 0,
            input_mode: None,
            available_harnesses: vec![],
            harness_picker_index: 0,
            input_harness_id: String::new(),
            input_working_dir: String::new(),
            input_role: String::new(),
            input_brief: String::new(),
            pane_content: String::new(),
            pane_cursor: (0, 0),
            focused: false,
            prefix_pending: false,
            status_message: None,
            hire_started_at: HashMap::new(),
            started_at: Instant::now(),
            animations_enabled: true,
            quit_requested_at: None,
            update_available: None,
        }
    }

    /// Registers a quit request from any source ('q', Ctrl+C, or an external
    /// SIGINT/SIGTERM). Returns `true` if this confirms an exit — a prior request is
    /// still within [`QUIT_CONFIRM_WINDOW`] — in which case the caller should actually
    /// exit. Returns `false` if this is the first request (or the window on an earlier
    /// one had already expired), in which case it (re)arms the window and the caller
    /// should keep running, showing [`Self::quit_warning_active`]'s warning.
    pub fn request_quit(&mut self) -> bool {
        let now = Instant::now();
        if let Some(first) = self.quit_requested_at {
            if now.duration_since(first) <= QUIT_CONFIRM_WINDOW {
                return true;
            }
        }
        self.quit_requested_at = Some(now);
        false
    }

    /// Whether a "press again to quit" warning should currently be shown — a first
    /// quit request is still within its confirmation window.
    pub fn quit_warning_active(&self) -> bool {
        self.quit_requested_at
            .map(|first| Instant::now().duration_since(first) <= QUIT_CONFIRM_WINDOW)
            .unwrap_or(false)
    }

    /// Keeps [`Self::hire_started_at`] in sync with the current roster: records "now"
    /// the first time a session is observed in `Starting`, and drops the entry once the
    /// session either moves off `Starting` or disappears from the roster (released,
    /// exited, or otherwise no longer returned by the daemon). Call this after
    /// refreshing `self.sessions`.
    pub fn sync_hire_tracking(&mut self) {
        let still_starting: HashSet<&str> = self
            .sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Starting)
            .map(|s| s.id.as_str())
            .collect();

        for id in &still_starting {
            self.hire_started_at.entry((*id).to_string()).or_insert_with(Instant::now);
        }
        self.hire_started_at.retain(|id, _| still_starting.contains(id.as_str()));
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

    /// Begins the four-step inline hire prompt, starting on the harness picker.
    /// `harnesses` should be the current availability-filtered list from the daemon
    /// (`Request::ListHarnesses`), fetched by the caller just before this is called.
    /// `default_working_dir` pre-fills the working-directory step (typically reins' own
    /// current directory) so accepting the default is just pressing Enter. Returns
    /// `false` without entering the prompt if the harness list is empty — there's
    /// nothing to pick, so the caller should show a status message instead of opening
    /// a picker with zero options.
    pub fn start_hire_input(
        &mut self,
        harnesses: Vec<HarnessProfile>,
        default_working_dir: impl Into<String>,
    ) -> bool {
        if harnesses.is_empty() {
            return false;
        }
        self.available_harnesses = harnesses;
        self.harness_picker_index = 0;
        self.input_mode = Some(InputMode::HarnessId);
        self.clear_input_fields();
        self.input_working_dir = default_working_dir.into();
        true
    }

    /// Abandons the inline hire prompt without sending anything.
    pub fn cancel_input(&mut self) {
        self.input_mode = None;
        self.clear_input_fields();
    }

    fn clear_input_fields(&mut self) {
        self.input_harness_id.clear();
        self.input_working_dir.clear();
        self.input_role.clear();
        self.input_brief.clear();
    }

    /// Moves the harness picker's highlight forward, wrapping. No-op outside the
    /// picker step or with nothing to pick from.
    pub fn picker_next(&mut self) {
        if !self.available_harnesses.is_empty() {
            self.harness_picker_index =
                (self.harness_picker_index + 1) % self.available_harnesses.len();
        }
    }

    /// Moves the harness picker's highlight backward, wrapping. No-op outside the
    /// picker step or with nothing to pick from.
    pub fn picker_prev(&mut self) {
        if !self.available_harnesses.is_empty() {
            self.harness_picker_index = (self.harness_picker_index
                + self.available_harnesses.len()
                - 1)
                % self.available_harnesses.len();
        }
    }

    /// The harness profile currently highlighted in the picker, if any.
    pub fn picker_selected(&self) -> Option<&HarnessProfile> {
        self.available_harnesses.get(self.harness_picker_index)
    }

    /// Appends a character to whichever field of the hire prompt is active. The
    /// harness-id step is a picker, not free text, so this is a no-op there (and when
    /// the prompt isn't active at all).
    pub fn push_char(&mut self, c: char) {
        match self.input_mode {
            Some(InputMode::HarnessId) => {}
            Some(InputMode::WorkingDirectory) => self.input_working_dir.push(c),
            Some(InputMode::Role) => self.input_role.push(c),
            Some(InputMode::Brief) => self.input_brief.push(c),
            None => {}
        }
    }

    /// Removes the last character from whichever field of the hire prompt is active.
    /// No-op for the harness-id picker step, when the prompt isn't active, or when the
    /// field is already empty.
    pub fn backspace(&mut self) {
        match self.input_mode {
            Some(InputMode::HarnessId) => {}
            Some(InputMode::WorkingDirectory) => {
                self.input_working_dir.pop();
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

    /// Advances the hire prompt on Enter: harness pick → working directory → role →
    /// brief. Returns `None` while more fields remain. Enter on the harness-id step
    /// captures the picker's current highlight into `input_harness_id`. Enter on the
    /// brief field (empty or not — the brief is optional) finishes the prompt,
    /// resetting `input_mode` to `None` and returning the collected [`HireInput`]. If
    /// the prompt wasn't active, returns `None` and does nothing.
    pub fn advance_input(&mut self) -> Option<HireInput> {
        match self.input_mode {
            Some(InputMode::HarnessId) => {
                // `start_hire_input` refuses to enter this mode with an empty list, so
                // there is always a selected profile here in practice — but stay
                // defensive rather than assume.
                let Some(profile) = self.picker_selected() else {
                    self.input_mode = None;
                    return None;
                };
                self.input_harness_id = profile.id.clone();
                self.input_mode = Some(InputMode::WorkingDirectory);
                None
            }
            Some(InputMode::WorkingDirectory) => {
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
                    working_dir: self.input_working_dir.clone(),
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

    /// Enters focus mode on the currently selected session: from here, keystrokes are
    /// forwarded into that session's pane instead of driving reins' own roster
    /// navigation (see [`Self::prefix_pending`] for how to get back out). Returns
    /// `false` without entering focus mode if the hire prompt is open or there's no
    /// session selected to focus.
    pub fn enter_focus(&mut self) -> bool {
        if self.input_mode.is_some() || self.selected_session().is_none() {
            return false;
        }
        self.focused = true;
        true
    }

    /// Leaves focus mode, returning to normal roster navigation.
    pub fn exit_focus(&mut self) {
        self.focused = false;
        self.prefix_pending = false;
    }

    /// Whether reins is currently focused on a session's pane.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Arms the focus-mode prefix: the next keystroke is a reins command (see
    /// [`Self::take_prefix_pending`]) rather than being forwarded to the pane.
    pub fn arm_prefix(&mut self) {
        self.prefix_pending = true;
    }

    /// Consumes and returns whether a prefix keystroke is currently pending — `true`
    /// means the caller's current keystroke is the one to interpret as a reins command,
    /// after which the pending state is cleared either way.
    pub fn take_prefix_pending(&mut self) -> bool {
        std::mem::take(&mut self.prefix_pending)
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

    fn profile(id: &str) -> HarnessProfile {
        HarnessProfile {
            id: id.into(),
            display_name: id.into(),
            strengths: vec![],
            constraints: vec![],
            notes: String::new(),
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
    fn hire_input_collects_four_fields_and_resets() {
        let mut app = App::new();
        assert_eq!(app.input_mode, None);

        assert!(app.start_hire_input(
            vec![profile("claude-code"), profile("codex")],
            "/default/dir",
        ));
        assert_eq!(app.input_mode, Some(InputMode::HarnessId));
        assert_eq!(app.picker_selected().unwrap().id, "claude-code");
        assert_eq!(app.input_working_dir, "/default/dir", "pre-filled with the default");

        app.picker_next();
        assert_eq!(app.picker_selected().unwrap().id, "codex");
        app.picker_next(); // wraps back around
        assert_eq!(app.picker_selected().unwrap().id, "claude-code");

        // Enter on the picker captures the highlight and advances, no result yet.
        assert_eq!(app.advance_input(), None);
        assert_eq!(app.input_mode, Some(InputMode::WorkingDirectory));
        assert_eq!(app.input_harness_id, "claude-code");

        // The working-directory field starts pre-filled but can be edited like any
        // other free-text field.
        app.push_char('!');
        assert_eq!(app.input_working_dir, "/default/dir!");
        app.backspace();
        assert_eq!(app.input_working_dir, "/default/dir");

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
                harness_id: "claude-code".into(),
                working_dir: "/default/dir".into(),
                role: "rc".into(),
                brief: "hi".into(),
            })
        );
        assert_eq!(app.input_mode, None);
        assert_eq!(app.input_harness_id, "");
        assert_eq!(app.input_working_dir, "");
        assert_eq!(app.input_role, "");
        assert_eq!(app.input_brief, "");
    }

    #[test]
    fn start_hire_input_refuses_an_empty_harness_list() {
        let mut app = App::new();
        assert!(!app.start_hire_input(vec![], "/tmp"));
        assert_eq!(app.input_mode, None);
    }

    #[test]
    fn picker_next_and_prev_wrap_around() {
        let mut app = App::new();
        app.start_hire_input(vec![profile("a"), profile("b"), profile("c")], "/tmp");
        assert_eq!(app.harness_picker_index, 0);

        app.picker_prev();
        assert_eq!(app.harness_picker_index, 2, "prev from the first entry wraps to the last");

        app.picker_next();
        app.picker_next();
        assert_eq!(app.harness_picker_index, 1);
    }

    #[test]
    fn brief_is_optional_and_completes_the_prompt_when_left_empty() {
        let mut app = App::new();
        app.start_hire_input(vec![profile("c")], "/tmp");
        app.advance_input(); // HarnessId -> WorkingDirectory
        app.advance_input(); // WorkingDirectory -> Role
        app.advance_input(); // Role -> Brief
        assert_eq!(app.input_mode, Some(InputMode::Brief));

        let result = app.advance_input().expect("empty brief still completes the prompt");
        assert_eq!(result.brief, "");
        assert_eq!(result.harness_id, "c");
        assert_eq!(result.working_dir, "/tmp");
        assert_eq!(app.input_mode, None);
    }

    #[test]
    fn first_quit_request_arms_the_warning_without_confirming() {
        let mut app = App::new();
        assert!(!app.quit_warning_active());

        let confirmed = app.request_quit();

        assert!(!confirmed, "a lone request shouldn't confirm exit");
        assert!(app.quit_warning_active());
    }

    #[test]
    fn second_quit_request_within_the_window_confirms_exit() {
        let mut app = App::new();
        app.request_quit();

        let confirmed = app.request_quit();

        assert!(confirmed, "a second request within the window should confirm exit");
    }

    #[test]
    fn quit_warning_is_inactive_before_any_request() {
        let app = App::new();
        assert!(!app.quit_warning_active());
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
    fn update_available_starts_unset() {
        let app = App::new();
        assert_eq!(app.update_available, None);
    }

    #[test]
    fn cancel_input_clears_state_without_returning_a_result() {
        let mut app = App::new();
        app.start_hire_input(vec![profile("c")], "/tmp");
        app.advance_input(); // captures "c" into input_harness_id, moves to WorkingDirectory
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

    #[test]
    fn enter_focus_requires_a_selected_session() {
        let mut app = App::new();
        assert!(app.sessions.is_empty());
        assert!(!app.enter_focus(), "nothing selected, nothing to focus");
        assert!(!app.is_focused());
    }

    #[test]
    fn enter_focus_refuses_while_the_hire_prompt_is_open() {
        let mut app = App::new();
        app.sessions = vec![session("a")];
        app.start_hire_input(vec![profile("c")], "/tmp");

        assert!(!app.enter_focus(), "hire prompt is open, shouldn't also enter focus mode");
        assert!(!app.is_focused());
    }

    #[test]
    fn enter_and_exit_focus_round_trip() {
        let mut app = App::new();
        app.sessions = vec![session("a")];

        assert!(app.enter_focus());
        assert!(app.is_focused());

        app.exit_focus();
        assert!(!app.is_focused());
    }

    #[test]
    fn exit_focus_also_clears_a_pending_prefix() {
        let mut app = App::new();
        app.sessions = vec![session("a")];
        app.enter_focus();
        app.arm_prefix();

        app.exit_focus();

        // A defocus-then-refocus shouldn't leave a stale prefix armed from before.
        assert!(!app.take_prefix_pending());
    }

    #[test]
    fn take_prefix_pending_consumes_the_armed_state() {
        let mut app = App::new();
        assert!(!app.take_prefix_pending(), "nothing armed yet");

        app.arm_prefix();
        assert!(app.take_prefix_pending(), "armed prefix should be reported once");
        assert!(!app.take_prefix_pending(), "and only once");
    }

    fn session_with_status(id: &str, status: SessionStatus) -> Session {
        let mut s = session(id);
        s.status = status;
        s
    }

    #[test]
    fn sync_hire_tracking_records_starting_sessions() {
        let mut app = App::new();
        app.sessions = vec![session_with_status("a", SessionStatus::Starting)];
        app.sync_hire_tracking();
        assert!(app.hire_started_at.contains_key("a"));
    }

    #[test]
    fn sync_hire_tracking_clears_entries_once_a_session_leaves_starting() {
        let mut app = App::new();
        app.sessions = vec![session_with_status("a", SessionStatus::Starting)];
        app.sync_hire_tracking();
        assert!(app.hire_started_at.contains_key("a"));

        app.sessions = vec![session_with_status("a", SessionStatus::Running)];
        app.sync_hire_tracking();
        assert!(!app.hire_started_at.contains_key("a"));
    }

    #[test]
    fn sync_hire_tracking_clears_entries_for_sessions_dropped_from_the_roster() {
        let mut app = App::new();
        app.sessions = vec![session_with_status("a", SessionStatus::Starting)];
        app.sync_hire_tracking();
        assert!(app.hire_started_at.contains_key("a"));

        app.sessions = vec![];
        app.sync_hire_tracking();
        assert!(app.hire_started_at.is_empty());
    }

    #[test]
    fn sync_hire_tracking_does_not_reset_an_already_tracked_start_time() {
        let mut app = App::new();
        app.sessions = vec![session_with_status("a", SessionStatus::Starting)];
        app.sync_hire_tracking();
        let first = *app.hire_started_at.get("a").unwrap();

        app.sync_hire_tracking();
        let second = *app.hire_started_at.get("a").unwrap();
        assert_eq!(first, second);
    }
}
