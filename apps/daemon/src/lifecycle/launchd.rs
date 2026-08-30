//! Installs and controls the `reinsd` daemon via macOS launchd.
//!
//! Everything here shells out to `launchctl` via `std::process::Command`,
//! matching the established shelling-out style in [`crate::tmux::TmuxController`]
//! rather than linking against low-level system libraries.

use std::path::Path;

pub use super::LifecycleError;

const PLIST_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.reins.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exec_start}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#;

/// Writes the launchd plist and starts the `reinsd` daemon via `launchctl bootstrap`.
/// The daemon will automatically restart on failure and survive logout by default
/// (launchd user agents don't require a linger equivalent).
pub fn install_and_start(reinsd_path: &Path) -> Result<(), LifecycleError> {
    let plist_path = proto::launchd_plist_path()?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = PLIST_TEMPLATE.replace("{exec_start}", &reinsd_path.display().to_string());
    std::fs::write(&plist_path, content)?;
    let uid = current_uid()?;
    run(&["bootstrap", &format!("gui/{uid}"), &plist_path.display().to_string()])?;
    Ok(())
}

/// Whether the plist file is present, i.e. `install_and_start` has been run before.
///
/// This only checks for the plist file's existence, not whether the agent is
/// currently active — a stopped-but-installed agent is still "installed".
pub fn is_installed() -> bool {
    proto::launchd_plist_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Starts the `reinsd` daemon if it's installed. Returns `Ok(false)` (a no-op,
/// not an error) when it isn't installed, so callers on a first run can decide
/// whether to fall through to a full `install_and_start` themselves.
pub fn start_if_installed() -> Result<bool, LifecycleError> {
    if !is_installed() {
        return Ok(false);
    }
    let uid = current_uid()?;
    run(&["kickstart", &format!("gui/{uid}/dev.reins.daemon")])?;
    Ok(true)
}

/// Restarts the `reinsd` daemon if it's installed, so an in-place binary swap
/// takes effect immediately rather than waiting for the next login. Returns
/// `Ok(false)` (not an error) when it isn't installed — same convention as
/// [`start_if_installed`]. Uses `launchctl kickstart -k`, which kills the
/// existing instance (if any) and starts a fresh one in a single call, rather
/// than a separate stop/start pair.
pub fn restart_if_installed() -> Result<bool, LifecycleError> {
    if !is_installed() {
        return Ok(false);
    }
    let uid = current_uid()?;
    run(&["kickstart", "-k", &format!("gui/{uid}/dev.reins.daemon")])?;
    Ok(true)
}

/// Whether the `reinsd` daemon is currently active (running), as opposed to
/// merely installed. Used after a restart to sanity-check the new binary
/// actually came back up.
pub fn is_active() -> bool {
    let Ok(uid) = current_uid() else {
        return false;
    };
    std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/dev.reins.daemon")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Resolves the current user's UID by shelling out to `id -u` and parsing its output.
/// This avoids adding a dependency on `libc`/`nix` for a single system call.
fn current_uid() -> Result<u32, LifecycleError> {
    let output = std::process::Command::new("id")
        .arg("-u")
        .output()?;
    if !output.status.success() {
        return Err(LifecycleError::CommandFailed(
            "id -u".to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    let uid_str = String::from_utf8_lossy(&output.stdout);
    uid_str
        .trim()
        .parse::<u32>()
        .map_err(|_| {
            LifecycleError::CommandFailed(
                "id -u".to_string(),
                format!("could not parse UID from: {}", uid_str),
            )
        })
}

fn run(args: &[&str]) -> Result<(), LifecycleError> {
    let output = std::process::Command::new("launchctl").args(args).output()?;
    if !output.status.success() {
        return Err(LifecycleError::CommandFailed(
            format!("launchctl {}", args.join(" ")),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn launchctl_available() -> bool {
        std::process::Command::new("launchctl")
            .arg("list")
            .output()
            .is_ok()
    }

    /// Tears down anything `install_and_start` may have registered, best-effort.
    /// Called on every exit path of the test below (including panics via a guard),
    /// so a failing assertion never leaves a stray plist or daemon on the machine.
    struct Teardown;
    impl Drop for Teardown {
        fn drop(&mut self) {
            if let Ok(uid) = current_uid() {
                let _ = std::process::Command::new("launchctl")
                    .args(["bootout", &format!("gui/{uid}/dev.reins.daemon")])
                    .output();
            }
            if let Ok(plist_path) = proto::launchd_plist_path() {
                let _ = std::fs::remove_file(plist_path);
            }
        }
    }

    #[test]
    fn install_and_start_writes_a_real_plist_file() {
        if !launchctl_available() {
            eprintln!("skipping: launchctl not available");
            return;
        }
        // Make sure we're not clobbering (or later "cleaning up") a real reinsd plist
        // that happens to already be installed on the machine running this test.
        if is_installed() {
            eprintln!("skipping: a reinsd plist is already installed on this machine");
            return;
        }

        let _teardown = Teardown;

        // Throwaway ExecStart so this test doesn't need a real reinsd binary.
        let fake_exec = Path::new("/bin/true");
        install_and_start(fake_exec).expect("install_and_start should succeed");

        assert!(is_installed(), "plist file should exist after install");

        let plist_path = proto::launchd_plist_path().expect("launchd_plist_path should resolve");
        let content = std::fs::read_to_string(&plist_path).expect("plist file should be readable");
        assert!(content.contains("ExecStart=/bin/true") || content.contains("/bin/true"));
        assert!(content.contains("dev.reins.daemon"));
        assert!(content.contains("KeepAlive"));

        // Teardown (bootout, remove plist) runs via the `Teardown` guard's Drop impl above,
        // so it still happens if an assertion above panics.
    }

    #[test]
    fn current_uid_returns_a_valid_uid() {
        let uid = current_uid().expect("current_uid should succeed");
        assert!(uid > 0, "UID should be positive");
    }
}
