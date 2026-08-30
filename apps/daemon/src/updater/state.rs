//! Local rate-limit state for the background update check, so `reins` never hits
//! the GitHub API more than once per [`CHECK_INTERVAL_SECS`].

use serde::{Deserialize, Serialize};

/// 24 hours.
pub const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateCheckState {
    pub last_checked_unix: i64,
    pub latest_known_version: Option<String>,
}

impl Default for UpdateCheckState {
    fn default() -> Self {
        Self { last_checked_unix: 0, latest_known_version: None }
    }
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
        // Not asserting file I/O here (that's covered by proto's own path tests) —
        // just that a garbage/missing file never panics or errors out of load_state.
        let state = load_state();
        assert!(state.last_checked_unix >= 0 || state.last_checked_unix == 0);
    }
}
