use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("tmux command failed: {0}")]
    CommandFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct TmuxController;

impl TmuxController {
    fn run(&self, args: &[&str]) -> Result<String, TmuxError> {
        let output = Command::new("tmux").args(args).output()?;
        if !output.status.success() {
            return Err(TmuxError::CommandFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub fn new_session(&self, name: &str, cwd: &Path, command: Command) -> Result<(), TmuxError> {
        let program = command.get_program().to_string_lossy().to_string();
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let mut tmux_args = vec![
            "new-session".to_string(),
            "-d".to_string(),
            "-s".to_string(),
            name.to_string(),
            "-c".to_string(),
            cwd.to_string_lossy().to_string(),
            program,
        ];
        tmux_args.extend(args);
        let arg_refs: Vec<&str> = tmux_args.iter().map(|s| s.as_str()).collect();
        self.run(&arg_refs)?;

        // The harness CLI runs as a full terminal program in its own right (Claude
        // Code, Codex, etc.) and expects the same terminal capabilities it would get
        // outside tmux — focus-in/out reporting and mouse events in particular; without
        // these, harnesses that want them print their own "tmux focus-events off"-style
        // warnings. Scoped to just this session (no `-g`) so it doesn't change tmux
        // behavior for any of the user's other, unrelated tmux sessions on the same
        // server. Best-effort: an old tmux version that doesn't recognize one of these
        // options shouldn't fail the hire over a capability nicety.
        for (option, value) in [("focus-events", "on"), ("mouse", "on")] {
            if let Err(err) = self.run(&["set-option", "-t", name, option, value]) {
                eprintln!(
                    "reinsd: could not enable tmux option '{option}' for session '{name}': {err}"
                );
            }
        }

        Ok(())
    }

    pub fn session_exists(&self, name: &str) -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn send_keys(&self, name: &str, keys: &[u8]) -> Result<(), TmuxError> {
        let keys_str = String::from_utf8_lossy(keys).to_string();
        self.run(&["send-keys", "-t", name, &keys_str])?;
        Ok(())
    }

    /// Sends `text` into the pane verbatim (`send-keys -l`), so ordinary typed
    /// characters — including ones that happen to spell a tmux key name, like a user
    /// literally typing the word "Enter" — are never misread as a command.
    pub fn send_literal(&self, name: &str, text: &str) -> Result<(), TmuxError> {
        self.run(&["send-keys", "-t", name, "-l", "--", text])?;
        Ok(())
    }

    /// Sends one of tmux's own named keys (`"Enter"`, `"Left"`, `"C-c"`, `"M-b"`, ...)
    /// into the pane. Reusing tmux's own key-name vocabulary here means special keys
    /// (arrows, function keys, Ctrl/Alt combinations) don't need their own hand-rolled
    /// ANSI escape-sequence encoding — tmux already knows how to translate each of
    /// these into the right bytes for whatever's running in the pane.
    pub fn send_key_token(&self, name: &str, token: &str) -> Result<(), TmuxError> {
        self.run(&["send-keys", "-t", name, token])?;
        Ok(())
    }

    pub fn kill_session(&self, name: &str) -> Result<(), TmuxError> {
        if !self.session_exists(name) {
            return Ok(());
        }
        self.run(&["kill-session", "-t", name])?;
        Ok(())
    }

    pub fn capture_pane(&self, name: &str) -> Result<String, TmuxError> {
        self.run(&["capture-pane", "-t", name, "-p"])
    }

    /// Captures the pane with color/style escape codes intact (`-e`, unlike
    /// [`Self::capture_pane`]'s plain text — used for status-detection string matching,
    /// which wants to match on plain content, not escape bytes) plus the pane's real
    /// cursor position. `capture-pane` itself never encodes where the cursor actually
    /// is — confirmed against a real tmux session, its captured text is exactly the
    /// visible cell content and colors, nothing more — so cursor position comes from a
    /// second, separate query.
    pub fn capture_pane_live(&self, name: &str) -> Result<PaneCapture, TmuxError> {
        let text = self.run(&["capture-pane", "-e", "-t", name, "-p"])?;
        let cursor = self.run(&["display-message", "-p", "-t", name, "#{cursor_x},#{cursor_y}"])?;
        let (x, y) = parse_cursor(cursor.trim())
            .ok_or_else(|| TmuxError::CommandFailed(format!("unparseable cursor position: '{cursor}'")))?;
        Ok(PaneCapture { text, cursor_x: x, cursor_y: y })
    }
}

/// Pane content plus cursor position, for live rendering. See
/// [`TmuxController::capture_pane_live`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCapture {
    /// `capture-pane -e` output: visible cell content with SGR escape codes.
    pub text: String,
    pub cursor_x: u16,
    pub cursor_y: u16,
}

/// Parses `display-message`'s `"#{cursor_x},#{cursor_y}"` output, e.g. `"25,12"`.
fn parse_cursor(raw: &str) -> Option<(u16, u16)> {
    let (x, y) = raw.split_once(',')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

#[cfg(test)]
fn tmux_available() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn spawns_and_kills_a_real_tmux_session() {
        if !tmux_available() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let controller = TmuxController;
        let name = "reins-test-spawn-kill";
        let _ = controller.kill_session(name); // clean slate if a prior run left it

        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        controller.new_session(name, std::path::Path::new("/tmp"), cmd).unwrap();

        assert!(controller.session_exists(name));

        controller.kill_session(name).unwrap();
        assert!(!controller.session_exists(name));
    }

    #[test]
    fn send_literal_and_key_token_reach_a_real_pane() {
        if !tmux_available() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let controller = TmuxController;
        let name = "reins-test-send-keys";
        let _ = controller.kill_session(name);

        // `cat` echoes stdin back to the pane, so what we send is what we'll capture.
        let cmd = Command::new("cat");
        controller.new_session(name, std::path::Path::new("/tmp"), cmd).unwrap();

        controller.send_literal(name, "hello").unwrap();
        controller.send_key_token(name, "Enter").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));

        let captured = controller.capture_pane(name).unwrap();
        assert!(captured.contains("hello"), "captured pane: {captured:?}");

        controller.kill_session(name).unwrap();
    }

    #[test]
    fn capture_pane_live_returns_a_real_cursor_position() {
        if !tmux_available() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let controller = TmuxController;
        let name = "reins-test-capture-live";
        let _ = controller.kill_session(name);

        let cmd = Command::new("cat");
        controller.new_session(name, std::path::Path::new("/tmp"), cmd).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let capture = controller.capture_pane_live(name).unwrap();
        // A fresh pane's cursor sits at the top-left; the exact value matters less than
        // confirming the query round-trips into real numbers rather than erroring.
        assert_eq!(capture.cursor_x, 0);
        assert_eq!(capture.cursor_y, 0);

        controller.kill_session(name).unwrap();
    }

    #[test]
    fn parse_cursor_reads_the_display_message_format() {
        assert_eq!(parse_cursor("25,12"), Some((25, 12)));
        assert_eq!(parse_cursor("0,0"), Some((0, 0)));
        assert_eq!(parse_cursor("not-a-cursor"), None);
        assert_eq!(parse_cursor(""), None);
    }
}
