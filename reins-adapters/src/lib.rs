mod claude_code;
mod codex;
mod registry;

pub use claude_code::ClaudeCodeAdapterFactory;
pub use codex::CodexAdapterFactory;
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
    fn spawn_command(&self, ctx: &SpawnContext) -> Command;
    fn interrupt_keys(&self) -> &[u8];
    fn detect_status(&self, screen: &TerminalSnapshot) -> HarnessStatus;
    fn log_dir(&self, ctx: &SpawnContext) -> PathBuf;
    fn parse_log(&self, path: &Path) -> Vec<ConversationTurn>;
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}
