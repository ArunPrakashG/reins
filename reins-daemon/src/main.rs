mod rpc_server;
mod session_manager;
mod tmux;

use reins_core::HarnessProfile;

/// Harness profile TOML files, embedded into the binary at build time so the daemon
/// ships with them regardless of deployment layout (no need to locate a profiles/
/// directory at runtime relative to the executable).
const CLAUDE_CODE_PROFILE_TOML: &str = include_str!("../../reins-adapters/profiles/claude-code.toml");
const CODEX_PROFILE_TOML: &str = include_str!("../../reins-adapters/profiles/codex.toml");
const GEMINI_CLI_PROFILE_TOML: &str = include_str!("../../reins-adapters/profiles/gemini-cli.toml");

fn load_profiles() -> Vec<HarnessProfile> {
    [CLAUDE_CODE_PROFILE_TOML, CODEX_PROFILE_TOML, GEMINI_CLI_PROFILE_TOML]
        .iter()
        .map(|raw| toml::from_str::<HarnessProfile>(raw).expect("valid harness profile TOML"))
        .collect()
}

#[tokio::main]
async fn main() {
    let socket_path = std::env::temp_dir().join("reinsd.sock");
    let store = std::sync::Arc::new(reins_store::SqliteStore::open_in_memory().expect("open store"));
    let mut registry = reins_adapters::AdapterRegistry::new();
    registry.register(Box::new(reins_adapters::ClaudeCodeAdapterFactory));
    registry.register(Box::new(reins_adapters::CodexAdapterFactory));
    registry.register(Box::new(reins_adapters::GeminiCliAdapterFactory));
    let manager = std::sync::Arc::new(session_manager::SessionManager::new(
        registry,
        tmux::TmuxController,
        store,
    ));
    let profiles = std::sync::Arc::new(load_profiles());

    println!("reinsd starting on {}", socket_path.display());
    rpc_server::run_control_server(&socket_path, manager, profiles).await;
}
