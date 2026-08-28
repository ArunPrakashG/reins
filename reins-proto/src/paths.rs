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
}
