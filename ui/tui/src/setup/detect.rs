use adapters::AdapterRegistry;
use reins_core::HarnessProfile;

/// Detects the installed tmux version by running `tmux -V`.
/// Returns `Some(version_string)` if tmux is available, `None` otherwise.
fn detect_tmux() -> Option<String> {
    let output = std::process::Command::new("tmux").arg("-V").output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Report on system and configuration availability for the first-run setup wizard.
#[derive(Debug, Clone)]
pub struct DetectionReport {
    /// `Some(version_string)` if tmux is installed, `None` otherwise.
    pub tmux: Option<String>,
    /// Tuples of (harness_id, is_available).
    /// `is_available` is true if the harness CLI is runnable in the current environment.
    pub harnesses: Vec<(String, bool)>,
}

impl DetectionReport {
    /// Returns true if tmux is not available.
    /// Used by the wizard to determine if tmux-installation is required.
    pub fn tmux_missing(&self) -> bool {
        self.tmux.is_none()
    }

    /// Returns true if no harness CLI is available.
    /// Used by the wizard to determine if any harness can be used.
    pub fn no_harness_available(&self) -> bool {
        self.harnesses.iter().all(|(_, available)| !available)
    }
}

/// Performs detection of tmux and available harnesses.
///
/// # Arguments
/// * `registry` - The adapter registry to build harness adapters.
/// * `profiles` - The harness profiles to check availability for.
///
/// # Returns
/// A `DetectionReport` with the detected tmux version and harness availability.
pub fn detect(registry: &AdapterRegistry, profiles: &[HarnessProfile]) -> DetectionReport {
    let tmux = detect_tmux();
    let harnesses = profiles
        .iter()
        .filter_map(|profile| {
            let adapter = registry.build(&profile.id, profile.clone()).ok()?;
            Some((profile.id.clone(), adapter.is_available()))
        })
        .collect();
    DetectionReport { tmux, harnesses }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adapters::HarnessAdapter;
    use std::path::{Path, PathBuf};

    /// A fake adapter for testing that can be configured to report availability.
    struct FakeAdapter {
        profile: HarnessProfile,
        available: bool,
    }

    impl HarnessAdapter for FakeAdapter {
        fn id(&self) -> &'static str {
            "fake"
        }

        fn profile(&self) -> &HarnessProfile {
            &self.profile
        }

        fn program_name(&self) -> &'static str {
            "fake-harness"
        }

        fn spawn_command(&self, _ctx: &adapters::SpawnContext) -> std::process::Command {
            std::process::Command::new("fake")
        }

        fn interrupt_keys(&self) -> &[u8] {
            b"\x03"
        }

        fn detect_status(
            &self,
            _screen: &adapters::TerminalSnapshot,
        ) -> reins_core::HarnessStatus {
            reins_core::HarnessStatus::Idle
        }

        fn log_dir(&self, _ctx: &adapters::SpawnContext) -> PathBuf {
            PathBuf::new()
        }

        fn parse_log(&self, _path: &Path) -> Vec<reins_core::ConversationTurn> {
            Vec::new()
        }

        fn is_available(&self) -> bool {
            self.available
        }
    }

    /// A fake factory for creating fake adapters.
    struct FakeFactory {
        available: bool,
    }

    impl adapters::AdapterFactory for FakeFactory {
        fn id(&self) -> &'static str {
            "fake"
        }

        fn create(&self, profile: HarnessProfile) -> Box<dyn HarnessAdapter> {
            Box::new(FakeAdapter {
                profile,
                available: self.available,
            })
        }
    }

    fn test_profile(id: &str, name: &str) -> HarnessProfile {
        HarnessProfile {
            id: id.to_string(),
            display_name: name.to_string(),
            strengths: vec![],
            constraints: vec![],
            notes: String::new(),
        }
    }

    #[test]
    fn tmux_missing_returns_true_when_no_tmux() {
        let report = DetectionReport {
            tmux: None,
            harnesses: vec![],
        };
        assert!(report.tmux_missing());
    }

    #[test]
    fn tmux_missing_returns_false_when_tmux_present() {
        let report = DetectionReport {
            tmux: Some("tmux 3.3a".to_string()),
            harnesses: vec![],
        };
        assert!(!report.tmux_missing());
    }

    #[test]
    fn no_harness_available_returns_true_when_all_unavailable() {
        let report = DetectionReport {
            tmux: Some("tmux 3.3a".to_string()),
            harnesses: vec![
                ("harness1".to_string(), false),
                ("harness2".to_string(), false),
            ],
        };
        assert!(report.no_harness_available());
    }

    #[test]
    fn no_harness_available_returns_false_when_any_available() {
        let report = DetectionReport {
            tmux: Some("tmux 3.3a".to_string()),
            harnesses: vec![
                ("harness1".to_string(), false),
                ("harness2".to_string(), true),
            ],
        };
        assert!(!report.no_harness_available());
    }

    #[test]
    fn no_harness_available_returns_true_when_empty() {
        let report = DetectionReport {
            tmux: Some("tmux 3.3a".to_string()),
            harnesses: vec![],
        };
        assert!(report.no_harness_available());
    }

    fn tmux_available() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok()
    }

    #[test]
    fn detect_tmux_returns_some_when_tmux_installed() {
        if !tmux_available() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let result = detect_tmux();
        assert!(result.is_some());
        if let Some(version) = result {
            assert!(version.starts_with("tmux"));
        }
    }

    #[test]
    fn detect_with_all_available_harnesses() {
        let mut registry = AdapterRegistry::new();
        registry.register(Box::new(FakeFactory { available: true }));

        let profiles = vec![
            test_profile("fake", "Fake Harness"),
            test_profile("fake", "Another Fake"),
        ];

        let report = detect(&registry, &profiles);

        assert_eq!(report.harnesses.len(), 2);
        assert!(report.harnesses.iter().all(|(_, available)| *available));
        assert!(!report.no_harness_available());
    }

    #[test]
    fn detect_with_no_available_harnesses() {
        let mut registry = AdapterRegistry::new();
        registry.register(Box::new(FakeFactory { available: false }));

        let profiles = vec![
            test_profile("fake", "Fake Harness"),
            test_profile("fake", "Another Fake"),
        ];

        let report = detect(&registry, &profiles);

        assert_eq!(report.harnesses.len(), 2);
        assert!(report.harnesses.iter().all(|(_, available)| !*available));
        assert!(report.no_harness_available());
    }

    #[test]
    fn detect_with_empty_profiles() {
        let registry = AdapterRegistry::new();
        let profiles = vec![];

        let report = detect(&registry, &profiles);

        assert_eq!(report.harnesses.len(), 0);
        assert!(report.no_harness_available());
    }

    #[test]
    fn detect_filters_out_unknown_harnesses() {
        let registry = AdapterRegistry::new(); // No factories registered

        let profiles = vec![
            test_profile("unknown", "Unknown Harness"),
            test_profile("also_unknown", "Another Unknown"),
        ];

        let report = detect(&registry, &profiles);

        // Unknown harnesses are filtered out, so the report should be empty
        assert_eq!(report.harnesses.len(), 0);
        assert!(report.no_harness_available());
    }
}
