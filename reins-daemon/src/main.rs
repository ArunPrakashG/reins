use anyhow::Context;
use reins_core::HarnessProfile;
use reins_daemon::{rpc_server, session_manager, tmux};

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

/// Resolves the on-disk location of the session database:
/// `$XDG_DATA_HOME/reins/reins.db`, falling back to `~/.local/share/reins/reins.db`.
/// Creates the parent directory if it doesn't exist.
fn store_path() -> anyhow::Result<std::path::PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                anyhow::anyhow!("neither XDG_DATA_HOME nor HOME is set; cannot locate a data directory")
            })?;
            std::path::PathBuf::from(home).join(".local/share")
        }
    };
    let dir = base.join("reins");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating data directory {}", dir.display()))?;
    Ok(dir.join("reins.db"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path =
        reins_proto::control_socket_path().context("resolving the control socket path")?;
    let db_path = store_path().context("resolving the session store path")?;
    let store = std::sync::Arc::new(
        reins_store::SqliteStore::open(&db_path)
            .with_context(|| format!("opening session store at {}", db_path.display()))?,
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

    // The roster now persists across restarts, but tmux sessions can die while the
    // daemon is down — reconcile before serving so we never report a dead session as
    // live.
    let reconciled = manager
        .reconcile_with_tmux()
        .context("reconciling stored sessions against tmux at startup")?;
    if reconciled > 0 {
        println!("reinsd: marked {reconciled} stale session(s) as exited");
    }

    println!("reinsd store: {}", db_path.display());
    println!("reinsd starting on {}", socket_path.display());
    rpc_server::run_control_server(&socket_path, manager, profiles)
        .await
        .with_context(|| format!("running control server on {}", socket_path.display()))?;
    Ok(())
}
