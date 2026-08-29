//! OS-specific service-manager lifecycle glue for `reinsd`.
//!
//! Each platform module shells out to that platform's user-level service manager
//! (`systemctl --user` on Linux, `launchctl` on macOS) to install, start, and query
//! the daemon as a background service, matching the shelling-out style established by
//! [`crate::tmux::TmuxController`].

#[cfg(target_os = "linux")]
pub mod systemd;
#[cfg(target_os = "macos")]
pub mod launchd;
