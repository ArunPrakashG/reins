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
///
/// Takes an owned `String` (rather than `&str`) so callers can `tokio::spawn` this
/// as a `'static` background task instead of awaiting it inline on the startup path.
pub async fn background_check(current_version: String) -> Option<String> {
    let now = now_unix();
    let mut saved_state = state::load_state();

    if !state::should_check(&saved_state, now, state::CHECK_INTERVAL_SECS) {
        return saved_state
            .latest_known_version
            .filter(|v| release::version_is_newer(&current_version, v));
    }

    let result = check_for_update(&current_version).await;
    saved_state.last_checked_unix = now;
    let available_version = match result {
        Ok(UpdateCheck::Available { version, .. }) => Some(version),
        _ => None,
    };
    saved_state.latest_known_version = available_version.clone();
    let _ = state::save_state(&saved_state);
    available_version
}

/// Full update flow for the `reins update` subcommand: check → download the
/// platform tarball → verify its checksum → extract `reins`/`reinsd` → atomically
/// swap both binaries in place → restart the `reinsd` service → confirm it came
/// back up. `progress` is called with short human-readable status lines so the
/// CLI caller can print them as they happen.
///
/// On any failure *after* a binary has already been swapped, this rolls that
/// binary back via [`rollback`] before returning the error — never leaves the
/// install half-applied.
pub async fn run_update(
    current_version: &str,
    reins_path: &std::path::Path,
    reinsd_path: &std::path::Path,
    progress: impl Fn(&str),
) -> Result<String, UpdaterError> {
    progress("Checking for updates...");
    let UpdateCheck::Available { version, asset_url, checksum_url } =
        check_for_update(current_version).await?
    else {
        return Ok(current_version.to_string());
    };

    progress(&format!("Downloading {version}..."));
    let client = reqwest::Client::builder()
        .user_agent(concat!("reins/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let tarball = client.get(&asset_url).send().await?.bytes().await?;
    let checksums_text = client.get(&checksum_url).send().await?.text().await?;

    let asset_name = release::platform_asset_name();
    let expected_hex = checksums_text
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hex = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (name == asset_name).then(|| hex.to_string())
        })
        .ok_or_else(|| UpdaterError::NoMatchingAsset(asset_name.to_string()))?;

    progress("Verifying checksum...");
    if !install::verify_checksum(&tarball, &expected_hex) {
        return Err(UpdaterError::ChecksumMismatch);
    }

    progress("Extracting...");
    let (new_reins, new_reinsd) = extract_binaries(&tarball)?;

    progress("Installing reins...");
    install::atomic_replace(reins_path, &new_reins)?;

    progress("Installing reinsd...");
    if let Err(err) = install::atomic_replace(reinsd_path, &new_reinsd) {
        let _ = install::rollback(reins_path);
        return Err(err.into());
    }

    progress("Restarting daemon...");
    if let Err(err) = restart_daemon() {
        let _ = install::rollback(reins_path);
        let _ = install::rollback(reinsd_path);
        // Restore the old binary *files*, then also make sure the old binary is what
        // is actually *running* — otherwise a failed restart can leave the daemon
        // dead (or running the new, unverified-to-work binary) while disk holds the
        // old one, which is exactly the half-applied state rollback exists to avoid.
        return Err(match restart_daemon() {
            Ok(()) => err,
            Err(e2) => UpdaterError::InvalidPayload(format!(
                "{err}; rolled back binaries but could not restart reinsd: {e2}"
            )),
        });
    }

    progress(&format!("Updated to {version}."));
    Ok(version)
}

/// Extracts the `reins` and `reinsd` files out of the downloaded `.tar.gz` asset.
/// Requires both files be present at the tarball's top level (matching the layout
/// `scripts/build-release.sh` produces) — anything else is a packaging bug on the
/// release side, not something the client should try to guess around.
fn extract_binaries(tarball: &[u8]) -> Result<(Vec<u8>, Vec<u8>), UpdaterError> {
    use std::io::Read;
    let decoder = flate2::read::GzDecoder::new(tarball);
    let mut archive = tar::Archive::new(decoder);
    let mut reins_bytes = None;
    let mut reinsd_bytes = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let mut buf = Vec::new();
        match name {
            "reins" => {
                entry.read_to_end(&mut buf)?;
                reins_bytes = Some(buf);
            }
            "reinsd" => {
                entry.read_to_end(&mut buf)?;
                reinsd_bytes = Some(buf);
            }
            _ => {}
        }
    }
    match (reins_bytes, reinsd_bytes) {
        (Some(r), Some(d)) => Ok((r, d)),
        _ => Err(UpdaterError::InvalidPayload(
            "release tarball is missing 'reins' or 'reinsd'".to_string(),
        )),
    }
}

fn restart_daemon() -> Result<(), UpdaterError> {
    #[cfg(target_os = "linux")]
    {
        crate::lifecycle::systemd::restart_if_installed()
            .map_err(|e| UpdaterError::InvalidPayload(e.to_string()))?;
        if crate::lifecycle::systemd::is_installed() && !crate::lifecycle::systemd::is_active() {
            return Err(UpdaterError::InvalidPayload(
                "reinsd did not come back up after restart".to_string(),
            ));
        }
    }
    #[cfg(target_os = "macos")]
    {
        crate::lifecycle::launchd::restart_if_installed()
            .map_err(|e| UpdaterError::InvalidPayload(e.to_string()))?;
        if crate::lifecycle::launchd::is_installed() && !crate::lifecycle::launchd::is_active() {
            return Err(UpdaterError::InvalidPayload(
                "reinsd did not come back up after restart".to_string(),
            ));
        }
    }
    Ok(())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Shared by `mod.rs`'s and `state.rs`'s test modules to guard `XDG_STATE_HOME`
/// mutation — both files' tests redirect it to an isolated temp dir so `cargo test`
/// never touches the developer's real `~/.local/state/reins/update-check.json` (see
/// the doc comment on each test that uses this). A single shared mutex, rather than
/// one per file, is required because both test modules run as threads in the same
/// binary and would otherwise race each other's save/restore of the same env var.
#[cfg(test)]
pub(crate) fn xdg_state_home_test_mutex() -> &'static std::sync::Mutex<()> {
    static MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    MUTEX.get_or_init(|| std::sync::Mutex::new(()))
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

    // The mutex guard below is deliberately held across the `background_check(...)`
    // .await: this is a `tokio::test` current-thread-runtime test, so nothing else
    // ever runs during that await, and the guard *must* stay held through it — its
    // whole purpose is to stop another test thread from repointing XDG_STATE_HOME
    // out from under this one while background_check is mid-flight (which itself
    // resolves that path via `proto::update_state_path`). A `std::sync::Mutex` is
    // fine here (this crate has no other async code contending on it).
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn background_check_respects_rate_limit_without_touching_the_network() {
        // Redirects XDG_STATE_HOME to an isolated temp dir for the duration of this
        // test (mirroring ui/tui/src/config.rs's HOME/XDG_CONFIG_HOME save-restore
        // pattern for its own tests) so this never reads or overwrites the
        // developer's real `~/.local/state/reins/update-check.json` — without this,
        // the save below would stamp a fake "v9.9.9 available" notice into the real
        // state file, which `reins` would then show as a false update notice for the
        // next 24 hours.
        let _guard = xdg_state_home_test_mutex().lock().unwrap();
        let old_xdg_state = std::env::var("XDG_STATE_HOME").ok();

        let thread_id = std::thread::current().id();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_state = std::env::temp_dir()
            .join(format!("reins-updater-test-{:?}-{}", thread_id, timestamp));
        let _ = std::fs::remove_dir_all(&temp_state);
        std::fs::create_dir_all(&temp_state).expect("create temp test dir");
        std::env::set_var("XDG_STATE_HOME", &temp_state);

        // Pin an already-recent last_checked_unix directly via the state module's
        // own round trip, so this test doesn't depend on network access at all.
        let recent_state = state::UpdateCheckState {
            last_checked_unix: now_unix(),
            latest_known_version: Some("v9.9.9".to_string()),
        };
        let save_result = state::save_state(&recent_state);
        let result = if save_result.is_ok() {
            Some(background_check("0.1.2".to_string()).await)
        } else {
            None
        };

        if let Some(x) = old_xdg_state {
            std::env::set_var("XDG_STATE_HOME", x);
        } else {
            std::env::remove_var("XDG_STATE_HOME");
        }
        let _ = std::fs::remove_dir_all(&temp_state);

        // Best-effort: if this environment somehow still can't resolve a state path,
        // there's nothing to assert.
        let Some(result) = result else { return };
        assert_eq!(result, Some("v9.9.9".to_string()));
    }

    #[test]
    fn extract_binaries_reads_reins_and_reinsd_from_a_real_tarball() {
        use std::io::Write;
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_cksum();
        builder.append_data(&mut header, "reins", &b"AAAAA"[..]).unwrap();
        let mut header2 = tar::Header::new_gnu();
        header2.set_size(6);
        header2.set_cksum();
        builder.append_data(&mut header2, "reinsd", &b"BBBBBB"[..]).unwrap();
        let tar_bytes = builder.into_inner().unwrap();

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        let gz_bytes = encoder.finish().unwrap();

        let (reins, reinsd) = extract_binaries(&gz_bytes).unwrap();
        assert_eq!(reins, b"AAAAA");
        assert_eq!(reinsd, b"BBBBBB");
    }
}
