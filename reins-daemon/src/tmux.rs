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
}

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
}
