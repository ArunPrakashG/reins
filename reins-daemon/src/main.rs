mod rpc_server;
mod session_manager;
mod tmux;

use anyhow::Context;
use reins_core::HarnessProfile;

/// Harness profile TOML files, embedded into the binary at build time so the daemon
/// ships with them regardless of deployment layout (no need to locate a profiles/
/// directory at runtime relative to the executable).
const CLAUDE_CODE_PROFILE_TOML: &str = include_str!("../../reins-adapters/profiles/claude-code.toml");
const CODEX_PROFILE_TOML: &str = include_str!("../../reins-adapters/profiles/codex.toml");
const GEMINI_CLI_PROFILE_TOML: &str = include_str!("../../reins-adapters/profiles/gemini-cli.toml");

fn load_profiles() -> anyhow::Result<Vec<HarnessProfile>> {
    [
        ("claude-code.toml", CLAUDE_CODE_PROFILE_TOML),
        ("codex.toml", CODEX_PROFILE_TOML),
        ("gemini-cli.toml", GEMINI_CLI_PROFILE_TOML),
    ]
    .iter()
    .map(|(name, raw)| {
        toml::from_str::<HarnessProfile>(raw)
            .with_context(|| format!("parsing embedded harness profile '{name}'"))
    })
    .collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path = std::env::temp_dir().join("reinsd.sock");
    let store = std::sync::Arc::new(
        reins_store::SqliteStore::open_in_memory().context("opening in-memory session store")?,
    );
    let mut registry = reins_adapters::AdapterRegistry::new();
    registry.register(Box::new(reins_adapters::ClaudeCodeAdapterFactory));
    registry.register(Box::new(reins_adapters::CodexAdapterFactory));
    registry.register(Box::new(reins_adapters::GeminiCliAdapterFactory));
    let manager = std::sync::Arc::new(session_manager::SessionManager::new(
        registry,
        tmux::TmuxController,
        store,
    ));
    let profiles = std::sync::Arc::new(load_profiles().context("loading harness profiles")?);

    println!("reinsd starting on {}", socket_path.display());
    rpc_server::run_control_server(&socket_path, manager, profiles)
        .await
        .with_context(|| format!("running control server on {}", socket_path.display()))?;
    Ok(())
}
