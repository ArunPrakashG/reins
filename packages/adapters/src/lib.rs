#[path = "impl/claude_code.rs"]
mod claude_code;
#[path = "impl/codex.rs"]
mod codex;
#[path = "impl/gemini_cli.rs"]
mod gemini_cli;
mod registry;

pub use claude_code::ClaudeCodeAdapterFactory;
pub use codex::CodexAdapterFactory;
pub use gemini_cli::GeminiCliAdapterFactory;
pub use registry::{AdapterFactory, AdapterRegistry, RegistryError};

use reins_core::{ConversationTurn, HarnessProfile, HarnessStatus};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct SpawnContext {
    pub project_path: PathBuf,
    pub role: Option<String>,
    pub brief: Option<String>,
}

pub struct TerminalSnapshot {
    pub text: String,
}

pub trait HarnessAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn profile(&self) -> &HarnessProfile;
    /// The name of the CLI binary this adapter spawns (e.g. `"claude"`), as it
    /// would be resolved against `PATH`. Used by `spawn_command`'s default
    /// program choice and by `is_available`'s PATH check, so both agree on
    /// what "this harness" means without either having to construct the other.
    fn program_name(&self) -> &'static str;
    fn spawn_command(&self, ctx: &SpawnContext) -> Command;
    fn interrupt_keys(&self) -> &[u8];
    fn detect_status(&self, screen: &TerminalSnapshot) -> HarnessStatus;
    fn log_dir(&self, ctx: &SpawnContext) -> PathBuf;
    fn parse_log(&self, path: &Path) -> Vec<ConversationTurn>;

    /// Best-effort check that this harness's CLI is actually runnable.
    /// Default: resolve `program_name()` against PATH. Adapters may override
    /// for a more specific check.
    fn is_available(&self) -> bool {
        which(self.program_name())
    }
}

fn which(program: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| dir.join(program).is_file())
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestAdapter {
        program: &'static str,
        profile: HarnessProfile,
    }

    impl HarnessAdapter for TestAdapter {
        fn id(&self) -> &'static str {
            "test"
        }

        fn profile(&self) -> &HarnessProfile {
            &self.profile
        }

        fn program_name(&self) -> &'static str {
            self.program
        }

        fn spawn_command(&self, _ctx: &SpawnContext) -> Command {
            Command::new(self.program)
        }

        fn interrupt_keys(&self) -> &[u8] {
            b"\x03"
        }

        fn detect_status(&self, _screen: &TerminalSnapshot) -> HarnessStatus {
            HarnessStatus::Idle
        }

        fn log_dir(&self, _ctx: &SpawnContext) -> PathBuf {
            PathBuf::new()
        }

        fn parse_log(&self, _path: &Path) -> Vec<ConversationTurn> {
            Vec::new()
        }
    }

    fn test_profile() -> HarnessProfile {
        HarnessProfile {
            id: "test".into(),
            display_name: "Test".into(),
            strengths: vec![],
            constraints: vec![],
            notes: String::new(),
        }
    }

    #[test]
    fn is_available_true_for_a_program_that_exists_on_path() {
        let adapter = TestAdapter {
            program: "sh",
            profile: test_profile(),
        };
        assert!(adapter.is_available());
    }

    #[test]
    fn is_available_false_for_a_nonsense_program_name() {
        let adapter = TestAdapter {
            program: "definitely-not-a-real-binary-abcxyz123",
            profile: test_profile(),
        };
        assert!(!adapter.is_available());
    }
}
