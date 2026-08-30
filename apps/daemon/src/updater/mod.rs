//! Self-updater: checks GitHub Releases for a newer `reins`/`reinsd` build and,
//! when the user runs `reins update`, downloads, verifies, and atomically installs
//! it before restarting the daemon.
//!
//! Design constraints (see the plan doc this module implements):
//! - Update source is GitHub Releases only.
//! - Checking never installs anything — installation only happens via `run_update`,
//!   which only the `reins update` subcommand calls.
//! - `reins` and `reinsd` are versioned and released together in one tarball.

mod release;
mod state;
mod install;

pub use release::{ReleaseAsset, ReleaseInfo, UpdaterError};
