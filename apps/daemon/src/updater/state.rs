//! Local rate-limit state for the background update check, so `reins` never hits
//! the GitHub API more than once per [`CHECK_INTERVAL_SECS`].

use serde::{Deserialize, Serialize};

/// 24 hours.
pub const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateCheckState {
    pub last_checked_unix: i64,
    pub latest_known_version: Option<String>,
}

/// Never errors — a missing, unreadable, or corrupt state file just means "we've
/// never successfully checked before," which is a safe default (it just means the
/// next launch will check).
pub fn load_state() -> UpdateCheckState {
    let Ok(path) = proto::update_state_path() else {
        return UpdateCheckState::default();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return UpdateCheckState::default();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_state(state: &UpdateCheckState) -> std::io::Result<()> {
    let path = proto::update_state_path()?;
    let content = serde_json::to_string_pretty(state)
        .unwrap_or_else(|_| "{}".to_string());
    std::fs::write(&path, content)
}

pub fn should_check(state: &UpdateCheckState, now_unix: i64, interval_secs: i64) -> bool {
    now_unix - state.last_checked_unix >= interval_secs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Guards every test in this crate that mutates `XDG_STATE_HOME` (or reads
    /// through it) — shared with `super::super::tests` (this module's sibling test
    /// module in `mod.rs`) via [`crate::updater::xdg_state_home_test_mutex`] so the
    /// two modules' tests, which run as separate threads in the same binary, can't
    /// race each other's env var save/restore. Mirrors `ui/tui/src/config.rs`'s
    /// `test_mutex` pattern for the same class of problem.
    fn test_mutex() -> &'static Mutex<()> {
        crate::updater::xdg_state_home_test_mutex()
    }

    fn temp_state_dir() -> std::path::PathBuf {
        let thread_id = std::thread::current().id();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir()
            .join(format!("reins-state-test-{:?}-{}", thread_id, timestamp));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    #[test]
    fn should_check_true_when_never_checked() {
        let state = UpdateCheckState::default();
        assert!(should_check(&state, 1_000_000, CHECK_INTERVAL_SECS));
    }

    #[test]
    fn should_check_false_within_interval() {
        let state = UpdateCheckState { last_checked_unix: 1_000_000, latest_known_version: None };
        assert!(!should_check(&state, 1_000_000 + 60, CHECK_INTERVAL_SECS));
    }

    #[test]
    fn should_check_true_after_interval_elapses() {
        let state = UpdateCheckState { last_checked_unix: 1_000_000, latest_known_version: None };
        assert!(should_check(&state, 1_000_000 + CHECK_INTERVAL_SECS + 1, CHECK_INTERVAL_SECS));
    }

    #[test]
    fn load_state_defaults_when_home_points_nowhere_useful() {
        let _guard = test_mutex().lock().unwrap();
        let old_xdg_state = std::env::var("XDG_STATE_HOME").ok();

        // Point XDG_STATE_HOME at a fresh, empty temp dir — no `reins/update-check.json`
        // exists there, so load_state must fall back to the default without touching
        // (or corrupting) the developer's real state file.
        let temp_state = temp_state_dir();
        std::env::set_var("XDG_STATE_HOME", &temp_state);

        let state = load_state();
        assert_eq!(state, UpdateCheckState::default());

        if let Some(x) = old_xdg_state {
            std::env::set_var("XDG_STATE_HOME", x);
        } else {
            std::env::remove_var("XDG_STATE_HOME");
        }
        let _ = std::fs::remove_dir_all(&temp_state);
    }
}
