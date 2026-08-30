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
pub use install::{sha256_hex, verify_checksum, atomic_replace, rollback};

const RELEASES_API_URL: &str = "https://api.github.com/repos/ArunPrakashG/reins/releases/latest";
const CHECKSUMS_ASSET_NAME: &str = "SHA256SUMS";

#[derive(Debug, Clone, PartialEq)]
pub enum UpdateCheck {
    UpToDate,
    Available { version: String, asset_url: String, checksum_url: String },
}

/// Performs a single HTTP call to the GitHub Releases API and compares the result
/// against `current_version`. Does no rate limiting or state I/O — see
/// [`background_check`] for the rate-limited wrapper used at startup.
pub async fn check_for_update(current_version: &str) -> Result<UpdateCheck, UpdaterError> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("reins/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let body = client.get(RELEASES_API_URL).send().await?.text().await?;
    let release = release::parse_release_json(&body)?;

    if !release::version_is_newer(current_version, &release.tag_name) {
        return Ok(UpdateCheck::UpToDate);
    }

    let asset_name = release::platform_asset_name();
    let asset = release::pick_asset(&release, asset_name)
        .ok_or_else(|| UpdaterError::NoMatchingAsset(asset_name.to_string()))?;
    let checksums = release::pick_asset(&release, CHECKSUMS_ASSET_NAME)
        .ok_or_else(|| UpdaterError::NoMatchingAsset(CHECKSUMS_ASSET_NAME.to_string()))?;

    Ok(UpdateCheck::Available {
        version: release.tag_name.clone(),
        asset_url: asset.browser_download_url.clone(),
        checksum_url: checksums.browser_download_url.clone(),
    })
}

/// Rate-limited wrapper around [`check_for_update`] for use on every `reins`
/// startup. Swallows every possible failure (network, GitHub API shape changes,
/// rate limiting) — a background check must never surprise the user with an
/// error or slow down startup. Returns `Some(version)` only when a genuinely
/// newer release is available and worth a status-line notice; the version string
/// is exactly the GitHub tag (e.g. `"v0.2.0"`).
pub async fn background_check(current_version: &str) -> Option<String> {
    let now = now_unix();
    let mut saved_state = state::load_state();

    if !state::should_check(&saved_state, now, state::CHECK_INTERVAL_SECS) {
        return saved_state.latest_known_version;
    }

    let result = check_for_update(current_version).await;
    saved_state.last_checked_unix = now;
    let available_version = match result {
        Ok(UpdateCheck::Available { version, .. }) => Some(version),
        _ => None,
    };
    saved_state.latest_known_version = available_version.clone();
    let _ = state::save_state(&saved_state);
    available_version
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits the real, public GitHub API (no auth needed for a public repo's
    /// `releases/latest`). Skips itself gracefully offline — this is the same
    /// tolerance pattern used by the `tmux`-dependent tests elsewhere in this
    /// crate for an unavailable external dependency.
    #[tokio::test]
    async fn check_for_update_against_the_real_repo_does_not_panic() {
        // The repo currently has zero published releases (confirmed via `gh release
        // list` while designing this feature), so GitHub returns 404 for
        // `releases/latest` today — this exercises the "network reachable but no
        // release yet" path and just asserts it doesn't panic or hang.
        let result = check_for_update("0.1.2").await;
        match result {
            Ok(_) => {}
            Err(_) => {} // 404 (no releases yet) or offline — both acceptable here
        }
    }

    #[tokio::test]
    async fn background_check_respects_rate_limit_without_touching_the_network() {
        // Pin an already-recent last_checked_unix directly via the state module's
        // own round trip, so this test doesn't depend on network access at all.
        let recent_state = state::UpdateCheckState {
            last_checked_unix: now_unix(),
            latest_known_version: Some("v9.9.9".to_string()),
        };
        // Best-effort: if this environment can't resolve a state path (no HOME),
        // there's nothing to assert.
        if state::save_state(&recent_state).is_err() {
            return;
        }
        let result = background_check("0.1.2").await;
        assert_eq!(result, Some("v9.9.9".to_string()));
    }
}
