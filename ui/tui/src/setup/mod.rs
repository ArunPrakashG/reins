pub mod detect;

use std::path::{Path, PathBuf};

/// Errors that can occur while running the first-run setup wizard.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("tmux not found — install it and run `reins` again")]
    TmuxMissing,
    #[error("no AI coding CLI found — install Claude Code, Codex CLI, or Gemini CLI and run `reins` again")]
    NoHarnessAvailable,
    #[error("daemon service install failed: {0}")]
    LifecycleError(String),
    #[error("linger permission needed — run: sudo reins --setup-linger")]
    LingerNeeded,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Runs the first-run setup wizard: detects tmux/harness availability (step 2-3),
/// installs and starts the `reinsd` background service for this platform (step 4-5),
/// and writes the setup-complete marker (step 6).
///
/// This intentionally keeps the "detect + gate on hard failures" prints separate from
/// the "install" logic below them (rather than interleaving early returns throughout)
/// so that a future caller — e.g. `reins setup`, which wants to re-detect and re-install
/// without exiting on the first non-fatal gap — can split this into two passes without
/// having to untangle print statements from control flow.
pub fn run_wizard(
    registry: &adapters::AdapterRegistry,
    profiles: &[reins_core::HarnessProfile],
) -> Result<(), SetupError> {
    let report = detect::detect(registry, profiles);

    if report.tmux_missing() {
        return Err(SetupError::TmuxMissing);
    }
    println!("tmux: {}", report.tmux.as_deref().unwrap_or(""));

    for (id, available) in &report.harnesses {
        println!("{id}: {}", if *available { "available" } else { "not found" });
    }
    if report.no_harness_available() {
        return Err(SetupError::NoHarnessAvailable);
    }

    let reinsd_path = resolve_reinsd_path()?;

    #[cfg(target_os = "linux")]
    {
        daemon::lifecycle::systemd::install_and_start(&reinsd_path)
            .map_err(|e| SetupError::LifecycleError(e.to_string()))?;
        if let Err(daemon::lifecycle::systemd::LifecycleError::LingerPermissionDenied) =
            daemon::lifecycle::systemd::enable_linger(&current_username()?)
        {
            return Err(SetupError::LingerNeeded);
        }
    }
    #[cfg(target_os = "macos")]
    {
        daemon::lifecycle::launchd::install_and_start(&reinsd_path)
            .map_err(|e| SetupError::LifecycleError(e.to_string()))?;
    }

    write_setup_marker()?;
    Ok(())
}

/// Resolves the path to the `reinsd` binary, which should be a sibling of the running
/// `reins` binary once both are installed together (e.g. via `cargo install` or a
/// packaged release placing both in the same bin directory). Falls back to searching
/// `PATH` if no sibling binary is found.
pub(crate) fn resolve_reinsd_path() -> Result<PathBuf, SetupError> {
    let current = std::env::current_exe()?;
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    resolve_reinsd_path_impl(&current, std::env::split_paths(&path_var))
}

/// Pure logic behind [`resolve_reinsd_path`], taking the current executable path and
/// the `PATH` search directories as parameters so it can be exercised in tests without
/// touching the real environment.
fn resolve_reinsd_path_impl(
    current_exe: &Path,
    path_dirs: impl Iterator<Item = PathBuf>,
) -> Result<PathBuf, SetupError> {
    let candidate = current_exe.with_file_name("reinsd");
    if candidate.exists() {
        return Ok(candidate);
    }
    path_dirs
        .map(|dir| dir.join("reinsd"))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            SetupError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "reinsd binary not found next to reins or on PATH",
            ))
        })
}

/// Runs `id -un` to determine the current user's login name, for `enable_linger`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn current_username() -> Result<String, SetupError> {
    let output = std::process::Command::new("id").arg("-un").output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Writes the empty setup-complete marker file so future launches skip the wizard.
fn write_setup_marker() -> Result<(), SetupError> {
    let path = proto::setup_marker_path()?;
    std::fs::write(&path, b"")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_reinsd_path_impl_finds_sibling_binary() {
        let dir = std::env::temp_dir().join(format!(
            "reins-setup-test-sibling-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let reinsd = dir.join("reinsd");
        std::fs::write(&reinsd, b"").unwrap();
        let current_exe = dir.join("reins");

        let result = resolve_reinsd_path_impl(&current_exe, std::iter::empty());

        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(result.unwrap(), reinsd);
    }

    #[test]
    fn resolve_reinsd_path_impl_falls_back_to_path() {
        let sibling_dir = std::env::temp_dir().join(format!(
            "reins-setup-test-nosibling-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&sibling_dir).unwrap();
        let current_exe = sibling_dir.join("reins");
        // No `reinsd` next to `current_exe`.

        let path_dir = std::env::temp_dir().join(format!(
            "reins-setup-test-pathdir-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path_dir).unwrap();
        let reinsd = path_dir.join("reinsd");
        std::fs::write(&reinsd, b"").unwrap();

        let result =
            resolve_reinsd_path_impl(&current_exe, std::iter::once(path_dir.clone()));

        std::fs::remove_dir_all(&sibling_dir).ok();
        std::fs::remove_dir_all(&path_dir).ok();
        assert_eq!(result.unwrap(), reinsd);
    }

    #[test]
    fn resolve_reinsd_path_impl_errors_when_not_found_anywhere() {
        let dir = std::env::temp_dir().join(format!(
            "reins-setup-test-notfound-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let current_exe = dir.join("reins");

        let result = resolve_reinsd_path_impl(&current_exe, std::iter::empty());

        std::fs::remove_dir_all(&dir).ok();
        assert!(result.is_err());
        match result.unwrap_err() {
            SetupError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("expected Io error, got {other:?}"),
        }
    }
}
