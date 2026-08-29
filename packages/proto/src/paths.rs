//! Shared filesystem locations for the reins control protocol.
//!
//! The daemon (`reinsd`) and the TUI (`reins`) must agree on where the control socket
//! lives, so the resolution rules live here in the protocol crate that both depend on
//! rather than being duplicated (and drifting) in each binary.

use std::io;
use std::path::PathBuf;

/// Filename of the daemon's JSON-RPC control socket, inside the directory chosen by
/// [`control_socket_path`].
pub const CONTROL_SOCKET_FILENAME: &str = "reinsd.sock";

/// Resolves the path of the daemon's control socket, creating its parent directory if
/// needed.
///
/// Resolution order:
/// 1. `$XDG_RUNTIME_DIR/reins/` — the per-user runtime directory, which the login
///    session manager already creates mode 0700 and owned by the user.
/// 2. `$HOME/.local/state/reins/` — created here with mode 0700 if it doesn't exist.
///
/// The world-writable system temp directory is deliberately *not* used: any local user
/// on a shared host could connect to a socket there and issue `Hire`, causing the
/// daemon to exec a harness CLI as the daemon's owner.
pub fn control_socket_path() -> io::Result<PathBuf> {
    let dir = control_socket_dir()?;
    ensure_private_dir(&dir)?;
    Ok(dir.join(CONTROL_SOCKET_FILENAME))
}

fn control_socket_dir() -> io::Result<PathBuf> {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !runtime_dir.is_empty() {
            return Ok(PathBuf::from(runtime_dir).join("reins"));
        }
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "neither XDG_RUNTIME_DIR nor HOME is set; cannot locate a private directory \
             for the reins control socket",
        )
    })?;
    Ok(PathBuf::from(home).join(".local/state/reins"))
}

/// Creates `dir` (and parents) if absent and forces its permissions to 0700, so that
/// only the owning user can reach the socket inside it.
fn ensure_private_dir(dir: &std::path::Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Resolves the path of the systemd user service unit file.
///
/// Returns: `~/.config/systemd/user/reinsd.service`
///
/// This path follows the XDG Base Directory specification for systemd user services.
pub fn systemd_unit_path() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "HOME is not set; cannot locate the systemd user service unit directory",
        )
    })?;
    Ok(PathBuf::from(home)
        .join(".config/systemd/user")
        .join("reinsd.service"))
}

/// Resolves the path of the launchd property list file.
///
/// Returns: `~/Library/LaunchAgents/dev.reins.daemon.plist`
///
/// This path follows macOS conventions for user launch agents.
pub fn launchd_plist_path() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "HOME is not set; cannot locate the launchd LaunchAgents directory",
        )
    })?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join("dev.reins.daemon.plist"))
}

/// Resolves the path of the setup completion marker file, creating its parent
/// directory if needed.
///
/// Resolution order:
/// 1. `$XDG_STATE_HOME/reins/setup-complete` — if `XDG_STATE_HOME` is set
/// 2. `~/.local/state/reins/setup-complete` — fallback using HOME
///
/// The parent directory is created with default permissions (not private) since
/// it is conventionally world-readable.
pub fn setup_marker_path() -> io::Result<PathBuf> {
    let dir = setup_marker_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("setup-complete"))
}

fn setup_marker_dir() -> io::Result<PathBuf> {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        if !state_home.is_empty() {
            return Ok(PathBuf::from(state_home).join("reins"));
        }
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "neither XDG_STATE_HOME nor HOME is set; cannot locate the setup marker directory",
        )
    })?;
    Ok(PathBuf::from(home).join(".local/state/reins"))
}

/// Resolves the path of the reins configuration file, creating its parent
/// directory if needed.
///
/// Resolution order:
/// 1. `$XDG_CONFIG_HOME/reins/config.toml` — if `XDG_CONFIG_HOME` is set
/// 2. `~/.config/reins/config.toml` — fallback using HOME
///
/// The parent directory is created with default permissions (not private) since
/// it is conventionally world-readable.
pub fn config_file_path() -> io::Result<PathBuf> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("config.toml"))
}

fn config_dir() -> io::Result<PathBuf> {
    if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME") {
        if !config_home.is_empty() {
            return Ok(PathBuf::from(config_home).join("reins"));
        }
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "neither XDG_CONFIG_HOME nor HOME is set; cannot locate the config directory",
        )
    })?;
    Ok(PathBuf::from(home).join(".config/reins"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The resolver must never hand back a path in the shared, world-writable temp
    /// directory (the pre-hardening behaviour this module replaced).
    #[test]
    fn resolved_socket_path_is_not_in_the_shared_temp_dir() {
        let Ok(path) = control_socket_path() else {
            // No XDG_RUNTIME_DIR and no HOME in this environment; nothing to assert.
            return;
        };
        let parent = path.parent().expect("socket path always has a parent");
        assert_ne!(parent, std::env::temp_dir());
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(CONTROL_SOCKET_FILENAME));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_forces_mode_0700() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("reins-paths-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .expect("loosen test dir");

        ensure_private_dir(&dir).expect("ensure_private_dir");

        let mode = std::fs::metadata(&dir).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn systemd_unit_path_ends_with_expected_filename() {
        let Ok(path) = systemd_unit_path() else {
            // No HOME in this environment; nothing to assert.
            return;
        };
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("reinsd.service"));
        assert!(path.to_string_lossy().contains(".config/systemd/user"));
    }

    #[test]
    fn launchd_plist_path_ends_with_expected_filename() {
        let Ok(path) = launchd_plist_path() else {
            // No HOME in this environment; nothing to assert.
            return;
        };
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("dev.reins.daemon.plist"));
        assert!(path.to_string_lossy().contains("Library/LaunchAgents"));
    }

    #[test]
    fn setup_marker_path_ends_with_expected_filename() {
        let Ok(path) = setup_marker_path() else {
            // No XDG_STATE_HOME or HOME in this environment; nothing to assert.
            return;
        };
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("setup-complete"));
        let parent = path.parent().expect("setup marker path always has a parent");
        assert!(parent.exists(), "setup marker parent directory should exist after calling setup_marker_path");
    }

    #[test]
    fn config_file_path_ends_with_expected_filename() {
        let Ok(path) = config_file_path() else {
            // No XDG_CONFIG_HOME or HOME in this environment; nothing to assert.
            return;
        };
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("config.toml"));
        let parent = path.parent().expect("config file path always has a parent");
        assert!(parent.exists(), "config file parent directory should exist after calling config_file_path");
    }
}
