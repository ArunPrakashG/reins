//! Installs and controls the `reinsd` daemon as a systemd `--user` service.
//!
//! Everything here shells out to `systemctl`/`loginctl` via `std::process::Command`,
//! matching the established shelling-out style in [`crate::tmux::TmuxController`]
//! rather than linking against a systemd client library.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("command '{0}' failed: {1}")]
    CommandFailed(String, String),
    #[error("permission denied enabling linger — run: sudo reins --setup-linger")]
    LingerPermissionDenied,
}

const UNIT_TEMPLATE: &str = r#"[Unit]
Description=Reins daemon (session manager for AI coding CLI harnesses)
After=default.target

[Service]
Type=simple
ExecStart={exec_start}
Restart=on-failure

[Install]
WantedBy=default.target
"#;

/// Writes the `reinsd.service` unit file and starts it via `systemctl --user enable
/// --now`, so it also comes back up on next login.
pub fn install_and_start(reinsd_path: &Path) -> Result<(), LifecycleError> {
    let unit_path = proto::systemd_unit_path()?;
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = UNIT_TEMPLATE.replace("{exec_start}", &reinsd_path.display().to_string());
    std::fs::write(&unit_path, content)?;
    run(&["--user", "daemon-reload"])?;
    run(&["--user", "enable", "--now", "reinsd"])?;
    Ok(())
}

/// Whether the unit file is present, i.e. `install_and_start` has been run before.
///
/// This only checks for the unit file's existence, not whether the service is
/// currently active — a stopped-but-installed service is still "installed".
pub fn is_installed() -> bool {
    proto::systemd_unit_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Starts the `reinsd` service if it's installed. Returns `Ok(false)` (a no-op,
/// not an error) when it isn't installed, so callers on a first run can decide
/// whether to fall through to a full `install_and_start` themselves.
pub fn start_if_installed() -> Result<bool, LifecycleError> {
    if !is_installed() {
        return Ok(false);
    }
    run(&["--user", "start", "reinsd"])?;
    Ok(true)
}

/// Enables "linger" for `user`, so their systemd `--user` instance (and therefore
/// `reinsd`) keeps running after their last login session ends, instead of being
/// torn down by systemd-logind.
///
/// `loginctl enable-linger` requires either running as root or being granted the
/// `org.freedesktop.login1.set-user-linger` polkit action; ordinary unprivileged
/// users are refused. See the module docs below for how that refusal is detected.
pub fn enable_linger(user: &str) -> Result<(), LifecycleError> {
    let output = std::process::Command::new("loginctl")
        .args(["enable-linger", user])
        .output()?;
    if !output.status.success() {
        // See the "linger permission detection" note below: any non-zero exit here
        // is treated as a permission failure rather than attempting to pattern-match
        // stderr, since that wording isn't stable across systemd versions/locales.
        return Err(LifecycleError::LingerPermissionDenied);
    }
    Ok(())
}

fn run(args: &[&str]) -> Result<(), LifecycleError> {
    let output = std::process::Command::new("systemctl").args(args).output()?;
    if !output.status.success() {
        return Err(LifecycleError::CommandFailed(
            format!("systemctl {}", args.join(" ")),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(())
}

// --- Linger permission detection ---------------------------------------------------
//
// The task brief's initial suggestion matched on stderr substrings ("Permission" /
// "not privileged") to distinguish a polkit/D-Bus permission refusal from a generic
// command failure. That was verified against this environment and rejected:
//
// - In this dev environment `loginctl enable-linger <user>` for the invoking user
//   actually *succeeds* (exit 0) — there's no local unprivileged-refusal case to
//   observe directly here to confirm wording.
// - Cross-referencing systemd's own source (`src/login/loginctl.c`): a refused
//   `SetUserLinger` D-Bus call surfaces through `bus_error_message`, which for a
//   polkit refusal is typically "Access denied" or "Interactive authentication
//   required.", not "Permission" or "not privileged". The brief's suggested
//   substrings would silently miss both of those real-world messages and misclassify
//   the failure as a generic `CommandFailed`.
// - Message text also isn't guaranteed stable across systemd versions or the caller's
//   locale (`loginctl` output can be localized), so pattern-matching stderr at all is
//   fragile.
//
// `enable_linger`'s only realistic non-zero-exit case in practice is a permission
// refusal (bad usernames, etc. are validated earlier by the caller/wizard), so per the
// brief's explicit fallback instruction this implementation treats *any* non-zero exit
// from `loginctl enable-linger` as `LingerPermissionDenied` rather than risking a
// misclassification as generic `CommandFailed` — the wizard's user-facing messaging
// differs meaningfully between the two (`LingerPermissionDenied` tells the user to run
// `sudo reins --setup-linger`; a generic failure doesn't).

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn systemctl_available() -> bool {
        std::process::Command::new("systemctl")
            .arg("--version")
            .output()
            .is_ok()
    }

    /// Tears down anything `install_and_start` may have registered, best-effort.
    /// Called on every exit path of the test below (including panics via a guard),
    /// so a failing assertion never leaves a stray `reinsd` unit on the machine.
    struct Teardown;
    impl Drop for Teardown {
        fn drop(&mut self) {
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "disable", "--now", "reinsd"])
                .output();
            if let Ok(unit_path) = proto::systemd_unit_path() {
                let _ = std::fs::remove_file(unit_path);
            }
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .output();
        }
    }

    #[test]
    fn install_and_start_writes_a_real_unit_file() {
        if !systemctl_available() {
            eprintln!("skipping: systemctl not available");
            return;
        }
        // Make sure we're not clobbering (or later "cleaning up") a real reinsd unit
        // that happens to already be installed on the machine running this test.
        if is_installed() {
            eprintln!("skipping: a reinsd unit is already installed on this machine");
            return;
        }

        let _teardown = Teardown;

        // Throwaway ExecStart so this test doesn't need a real reinsd binary.
        let fake_exec = Path::new("/bin/true");
        install_and_start(fake_exec).expect("install_and_start should succeed");

        assert!(is_installed(), "unit file should exist after install");

        let unit_path = proto::systemd_unit_path().expect("systemd_unit_path should resolve");
        let content = std::fs::read_to_string(&unit_path).expect("unit file should be readable");
        assert!(content.contains("ExecStart=/bin/true"));
        assert!(content.contains("Description=Reins daemon"));
        assert!(content.contains("WantedBy=default.target"));

        // Teardown (disable --now, remove unit, daemon-reload) runs via the `Teardown`
        // guard's Drop impl above, so it still happens if an assertion above panics.
    }
}
