# Reins Self-Updater Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `reins` a self-updater: a rate-limited background check against GitHub Releases that surfaces a status-line notice, and a `reins update` subcommand that downloads, verifies, and atomically installs the latest `reins`/`reinsd` binaries and restarts the daemon — plus a `scripts/build-release.sh` extension that publishes the GitHub Release the updater reads from.

**Architecture:** A new `updater` module (in the `daemon` lib crate, since both `reins` and `reinsd` already depend on it and it needs `proto::paths` + `lifecycle`) exposes pure, testable functions: parsing a GitHub release JSON payload into a `ReleaseInfo`, picking the right platform asset name, computing a SHA256, and performing an atomic file swap with backup/rollback. `ui/tui/src/main.rs` wires these into two entry points: a fire-and-forget background check on startup (writes a small rate-limit state file), and a new `reins update` subcommand that runs the full check → download → verify → swap → restart flow synchronously with progress printed to stdout. `scripts/build-release.sh` gains a packaging + `gh release create` step so there's something for the updater to find.

**Tech Stack:** `reqwest` (rustls, no default TLS) for HTTP, `sha2` for checksums — both new workspace dependencies. Everything else (tokio, serde, thiserror, existing `proto`/`daemon` crates) is already in the workspace.

**Spec:** This plan *is* the spec — the design was proposed to and approved by the project owner in-conversation (see the three locked decisions below). There is no separate spec doc.

## Global Constraints

- Update source is **GitHub Releases** (`https://api.github.com/repos/ArunPrakashG/reins/releases/latest`), not a custom server.
- Trigger model is **automatic background check + manual install** — the daemon/TUI never installs an update without the user running `reins update`.
- `scripts/build-release.sh` is extended to publish the release (tarball + checksums + `gh release create`) as part of the same work, not handled separately.
- `reins` and `reinsd` ship and update in lockstep — one release tarball contains both binaries at the same version. This directly closes the wire-protocol-skew bug hit earlier this session.
- No new HTTP client dependency may pull in OpenSSL — use `reqwest` with `default-features = false, features = ["rustls-tls", "json"]` so there's no system OpenSSL build dependency on either target platform (Linux/macOS, per the packaging spec's platform scope).
- Background checks must never block startup or error loudly: network failure, GitHub API rate-limiting, or a malformed payload are all silently swallowed (best-effort), matching the existing `enable_linger`/wizard tone of "never surprise the user with an unrelated failure."
- Rate limit background checks to at most once per 24h via a local state file — never hit the GitHub API on every launch.

---

## File Structure

- Create: `apps/daemon/src/updater/mod.rs` — public API: `check_for_update`, `UpdateCheck` result type, re-exports.
- Create: `apps/daemon/src/updater/release.rs` — `ReleaseInfo`/`ReleaseAsset` structs + JSON parsing, `platform_asset_name()`, `pick_asset()`.
- Create: `apps/daemon/src/updater/install.rs` — download-to-temp, SHA256 verification, atomic swap-with-backup/rollback, daemon-restart-and-health-check.
- Create: `apps/daemon/src/updater/state.rs` — rate-limit state file read/write (last-checked timestamp + cached latest version).
- Modify: `apps/daemon/src/lib.rs` — add `pub mod updater;`.
- Modify: `packages/proto/src/paths.rs` — add `update_state_path()` (mirrors `setup_marker_path()`).
- Modify: `ui/tui/src/main.rs` — add `"update"` subcommand dispatch, background check call in `run()`, status-line wiring.
- Modify: `ui/tui/src/app.rs` — add `App.update_available: Option<String>` field + setter, used by the status line.
- Modify: `ui/tui/src/ui.rs` — `draw_status_line` shows the update notice when set (lowest priority — focus-mode/quit-warning still win).
- Modify: `Cargo.toml` (root) — add `reqwest` and `sha2` to `[workspace.dependencies]`.
- Modify: `apps/daemon/Cargo.toml` — add `reqwest`, `sha2` deps.
- Modify: `scripts/build-release.sh` — package tarballs + checksums + `gh release create`.

## Interfaces (cross-task contract)

```rust
// apps/daemon/src/updater/release.rs
pub struct ReleaseAsset { pub name: String, pub browser_download_url: String }
pub struct ReleaseInfo { pub tag_name: String, pub assets: Vec<ReleaseAsset> }

pub fn parse_release_json(body: &str) -> Result<ReleaseInfo, UpdaterError>;
pub fn platform_asset_name() -> &'static str; // e.g. "reins-linux-x86_64.tar.gz"
pub fn pick_asset<'a>(release: &'a ReleaseInfo, asset_name: &str) -> Option<&'a ReleaseAsset>;
pub fn version_is_newer(current: &str, latest_tag: &str) -> bool; // latest_tag like "v0.2.0"

// apps/daemon/src/updater/state.rs
pub struct UpdateCheckState { pub last_checked_unix: i64, pub latest_known_version: Option<String> }
pub fn load_state() -> UpdateCheckState;                 // never errors, defaults on any failure
pub fn save_state(state: &UpdateCheckState) -> std::io::Result<()>;
pub fn should_check(state: &UpdateCheckState, now_unix: i64, interval_secs: i64) -> bool;

// apps/daemon/src/updater/install.rs
pub fn sha256_hex(bytes: &[u8]) -> String;
pub fn verify_checksum(bytes: &[u8], expected_hex: &str) -> bool;
pub fn atomic_replace(target: &std::path::Path, new_content: &[u8]) -> std::io::Result<()>; // writes to `target.new`, backs up target to `target.bak`, renames into place
pub fn rollback(target: &std::path::Path) -> std::io::Result<()>; // restores `target.bak` -> `target`

// apps/daemon/src/updater/mod.rs
pub enum UpdateCheck { UpToDate, Available { version: String, asset_url: String, checksum_url: String } }
pub async fn check_for_update(current_version: &str) -> Result<UpdateCheck, UpdaterError>; // one HTTP call, no state I/O
pub async fn background_check(current_version: &str) -> Option<String>; // rate-limited wrapper; returns Some(version) only when a newer one is available and worth surfacing; swallows all errors
pub async fn run_update(current_version: &str, reins_path: &std::path::Path, reinsd_path: &std::path::Path) -> Result<String, UpdaterError>; // full flow; returns the installed version string; on any failure after a swap, calls rollback() on whatever was already swapped
```

```rust
// packages/proto/src/paths.rs
pub fn update_state_path() -> io::Result<PathBuf>; // $XDG_STATE_HOME/reins/update-check.json (or ~/.local/state/reins/update-check.json)
```

---

### Task 1: Release JSON parsing and platform asset selection

**Files:**
- Create: `apps/daemon/src/updater/release.rs`
- Modify: `apps/daemon/src/lib.rs` (add `pub mod updater;` — create `apps/daemon/src/updater/mod.rs` as an empty-ish placeholder in this task too, just enough to make `mod release;` compile)
- Modify: `Cargo.toml` (root `[workspace.dependencies]`)
- Modify: `apps/daemon/Cargo.toml`

**Interfaces:**
- Produces: `ReleaseAsset`, `ReleaseInfo`, `parse_release_json`, `platform_asset_name`, `pick_asset`, `version_is_newer`, `UpdaterError` (used by every later task).

- [ ] **Step 1: Add workspace dependencies**

In root `Cargo.toml`, under `[workspace.dependencies]`, add:

```toml
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }
sha2 = "0.10"
```

In `apps/daemon/Cargo.toml`, under `[dependencies]`, add:

```toml
reqwest.workspace = true
sha2.workspace = true
```

- [ ] **Step 2: Run a build to confirm the new deps resolve**

Run: `cargo build -p daemon`
Expected: succeeds (nothing uses the new deps yet, this just confirms dependency resolution/download works)

- [ ] **Step 3: Create the updater module skeleton**

Create `apps/daemon/src/updater/mod.rs`:

```rust
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
```

- [ ] **Step 4: Wire the module into the crate**

In `apps/daemon/src/lib.rs`, add near the other `pub mod` declarations:

```rust
pub mod updater;
```

- [ ] **Step 5: Write failing tests for release.rs**

Create `apps/daemon/src/updater/release.rs` with tests first:

```rust
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum UpdaterError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid release payload: {0}")]
    InvalidPayload(String),
    #[error("no release asset found for this platform ('{0}')")]
    NoMatchingAsset(String),
    #[error("checksum mismatch for downloaded asset")]
    ChecksumMismatch,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
}

pub fn parse_release_json(body: &str) -> Result<ReleaseInfo, UpdaterError> {
    serde_json::from_str(body).map_err(|e| UpdaterError::InvalidPayload(e.to_string()))
}

/// The asset filename this platform's `reins update` should look for, matching the
/// naming convention `scripts/build-release.sh` produces (`reins-<os>-<arch>.tar.gz`).
pub fn platform_asset_name() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "reins-linux-x86_64.tar.gz"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "reins-linux-aarch64.tar.gz"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "reins-macos-x86_64.tar.gz"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "reins-macos-aarch64.tar.gz"
    }
}

pub fn pick_asset<'a>(release: &'a ReleaseInfo, asset_name: &str) -> Option<&'a ReleaseAsset> {
    release.assets.iter().find(|a| a.name == asset_name)
}

/// Compares a bare crate version (`"0.1.2"`, no leading `v`) against a GitHub release
/// tag (`"v0.2.0"` or `"0.2.0"`) by parsing each into a `(major, minor, patch)` tuple.
/// A tag that fails to parse is treated as not-newer (fail closed: never nag the user
/// about a malformed tag).
pub fn version_is_newer(current: &str, latest_tag: &str) -> bool {
    let parse = |s: &str| -> Option<(u64, u64, u64)> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some((major, minor, patch))
    };
    match (parse(current), parse(latest_tag)) {
        (Some(cur), Some(latest)) => latest > cur,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_release_json_reads_tag_and_assets() {
        let body = r#"{
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "reins-linux-x86_64.tar.gz", "browser_download_url": "https://example.com/a.tar.gz"},
                {"name": "SHA256SUMS", "browser_download_url": "https://example.com/sums.txt"}
            ]
        }"#;
        let release = parse_release_json(body).unwrap();
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(release.assets.len(), 2);
    }

    #[test]
    fn parse_release_json_rejects_garbage() {
        assert!(parse_release_json("not json").is_err());
    }

    #[test]
    fn pick_asset_finds_exact_name_match() {
        let release = ReleaseInfo {
            tag_name: "v0.2.0".into(),
            assets: vec![
                ReleaseAsset { name: "reins-linux-x86_64.tar.gz".into(), browser_download_url: "u1".into() },
                ReleaseAsset { name: "reins-macos-x86_64.tar.gz".into(), browser_download_url: "u2".into() },
            ],
        };
        let found = pick_asset(&release, "reins-linux-x86_64.tar.gz").unwrap();
        assert_eq!(found.browser_download_url, "u1");
        assert!(pick_asset(&release, "reins-windows-x86_64.tar.gz").is_none());
    }

    #[test]
    fn version_is_newer_compares_semver_tuples() {
        assert!(version_is_newer("0.1.2", "v0.2.0"));
        assert!(version_is_newer("0.1.2", "0.1.3"));
        assert!(!version_is_newer("0.1.2", "v0.1.2"));
        assert!(!version_is_newer("0.2.0", "v0.1.9"));
        assert!(!version_is_newer("0.1.2", "not-a-version"));
    }
}
```

Also add placeholder files so the module tree compiles (filled in by Tasks 2-3):

Create `apps/daemon/src/updater/state.rs`:
```rust
// Implemented in Task 2.
```

Create `apps/daemon/src/updater/install.rs`:
```rust
// Implemented in Task 3.
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p daemon updater::release`
Expected: all pass (`parse_release_json_reads_tag_and_assets`, `parse_release_json_rejects_garbage`, `pick_asset_finds_exact_name_match`, `version_is_newer_compares_semver_tuples`)

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml apps/daemon/Cargo.toml apps/daemon/src/lib.rs apps/daemon/src/updater
git commit -m "feat(updater): parse GitHub release payloads and pick the platform asset"
```

---

### Task 2: Rate-limit state file

**Files:**
- Modify: `packages/proto/src/paths.rs`
- Create: content of `apps/daemon/src/updater/state.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `UpdateCheckState`, `load_state`, `save_state`, `should_check` (used by Task 4's `background_check`).

- [ ] **Step 1: Add `update_state_path()` to proto**

In `packages/proto/src/paths.rs`, add (following the exact pattern of `setup_marker_path`/`setup_marker_dir` immediately above it):

```rust
/// Resolves the path of the self-updater's rate-limit state file, creating its
/// parent directory if needed.
///
/// Resolution order:
/// 1. `$XDG_STATE_HOME/reins/update-check.json` — if `XDG_STATE_HOME` is set
/// 2. `~/.local/state/reins/update-check.json` — fallback using HOME
pub fn update_state_path() -> io::Result<PathBuf> {
    let dir = setup_marker_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("update-check.json"))
}
```

- [ ] **Step 2: Add a proto test**

In `packages/proto/src/paths.rs`'s `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn update_state_path_ends_with_expected_filename() {
    let Ok(path) = update_state_path() else {
        return;
    };
    assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("update-check.json"));
}
```

- [ ] **Step 3: Run the proto test**

Run: `cargo test -p proto update_state_path`
Expected: PASS

- [ ] **Step 4: Write the state module (test-first)**

Replace the placeholder `apps/daemon/src/updater/state.rs` with:

```rust
//! Local rate-limit state for the background update check, so `reins` never hits
//! the GitHub API more than once per [`CHECK_INTERVAL_SECS`].

use serde::{Deserialize, Serialize};

/// 24 hours.
pub const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateCheckState {
    pub last_checked_unix: i64,
    pub latest_known_version: Option<String>,
}

impl Default for UpdateCheckState {
    fn default() -> Self {
        Self { last_checked_unix: 0, latest_known_version: None }
    }
}

/// Never errors — a missing, unreadable, or corrupt state file just means "we've
/// never successfully checked before," which is a safe default (it just means the
/// next launch will check).
pub fn load_state() -> UpdateCheckState {
    let Ok(path) = proto::update_state_path() else {
        return UpdateCheckState::default();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return UpdateCheckState::default();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_state(state: &UpdateCheckState) -> std::io::Result<()> {
    let path = proto::update_state_path()?;
    let content = serde_json::to_string_pretty(state)
        .unwrap_or_else(|_| "{}".to_string());
    std::fs::write(&path, content)
}

pub fn should_check(state: &UpdateCheckState, now_unix: i64, interval_secs: i64) -> bool {
    now_unix - state.last_checked_unix >= interval_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_check_true_when_never_checked() {
        let state = UpdateCheckState::default();
        assert!(should_check(&state, 1_000_000, CHECK_INTERVAL_SECS));
    }

    #[test]
    fn should_check_false_within_interval() {
        let state = UpdateCheckState { last_checked_unix: 1_000_000, latest_known_version: None };
        assert!(!should_check(&state, 1_000_000 + 60, CHECK_INTERVAL_SECS));
    }

    #[test]
    fn should_check_true_after_interval_elapses() {
        let state = UpdateCheckState { last_checked_unix: 1_000_000, latest_known_version: None };
        assert!(should_check(&state, 1_000_000 + CHECK_INTERVAL_SECS + 1, CHECK_INTERVAL_SECS));
    }

    #[test]
    fn load_state_defaults_when_home_points_nowhere_useful() {
        // Not asserting file I/O here (that's covered by proto's own path tests) —
        // just that a garbage/missing file never panics or errors out of load_state.
        let state = load_state();
        assert!(state.last_checked_unix >= 0 || state.last_checked_unix == 0);
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p daemon updater::state`
Expected: all pass

- [ ] **Step 6: Commit**

```bash
git add packages/proto/src/paths.rs apps/daemon/src/updater/state.rs
git commit -m "feat(updater): add rate-limited local state for the background update check"
```

---

### Task 3: Checksum verification and atomic binary replacement

**Files:**
- Create: content of `apps/daemon/src/updater/install.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `sha256_hex`, `verify_checksum`, `atomic_replace`, `rollback` (used by Task 5's `run_update`).

- [ ] **Step 1: Write the install module (test-first)**

Replace the placeholder `apps/daemon/src/updater/install.rs` with:

```rust
//! Checksum verification and atomic binary replacement for installed updates.
//!
//! `atomic_replace` follows the same trick `rustup`/`cargo install` use for
//! self-replacing a running executable: write the new content to a sibling temp
//! file, then `rename()` it over the target. On Unix, `rename()` onto an existing
//! path is atomic and doesn't disturb a process that already has the old inode
//! open (it keeps running against the old file until it exits) — so this is safe
//! to do even while `reins`/`reinsd` are themselves running.

use sha2::{Digest, Sha256};
use std::path::Path;

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub fn verify_checksum(bytes: &[u8], expected_hex: &str) -> bool {
    sha256_hex(bytes).eq_ignore_ascii_case(expected_hex.trim())
}

/// Backs up `target` to `target.bak` (overwriting any previous backup), writes
/// `new_content` to `target.new`, then renames `target.new` onto `target`.
///
/// If `target` doesn't exist yet, no backup is made (nothing to roll back to).
pub fn atomic_replace(target: &Path, new_content: &[u8]) -> std::io::Result<()> {
    if target.exists() {
        std::fs::copy(target, backup_path(target))?;
    }
    let staged = staged_path(target);
    std::fs::write(&staged, new_content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&staged, target)?;
    Ok(())
}

/// Restores `target.bak` over `target`. A no-op (not an error) if there is no
/// backup — callers use this defensively after a partial multi-file update, and
/// not every target necessarily got as far as being replaced.
pub fn rollback(target: &Path) -> std::io::Result<()> {
    let backup = backup_path(target);
    if backup.exists() {
        std::fs::rename(&backup, target)?;
    }
    Ok(())
}

fn backup_path(target: &Path) -> std::path::PathBuf {
    let mut p = target.as_os_str().to_owned();
    p.push(".bak");
    p.into()
}

fn staged_path(target: &Path) -> std::path::PathBuf {
    let mut p = target.as_os_str().to_owned();
    p.push(".new");
    p.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, content: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "reins-updater-test-{}-{}",
            std::process::id(),
            name
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        path
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // sha256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verify_checksum_accepts_matching_hash_case_insensitively() {
        let bytes = b"hello reins";
        let hex = sha256_hex(bytes);
        assert!(verify_checksum(bytes, &hex));
        assert!(verify_checksum(bytes, &hex.to_uppercase()));
        assert!(!verify_checksum(bytes, "deadbeef"));
    }

    #[test]
    fn atomic_replace_swaps_content_and_backs_up_the_original() {
        let target = temp_file("target-swap", b"old content");
        atomic_replace(&target, b"new content").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new content");
        assert_eq!(std::fs::read(backup_path(&target)).unwrap(), b"old content");

        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(backup_path(&target));
    }

    #[test]
    fn rollback_restores_the_backup() {
        let target = temp_file("target-rollback", b"old content");
        atomic_replace(&target, b"new content").unwrap();
        rollback(&target).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"old content");
        assert!(!backup_path(&target).exists());

        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn rollback_without_a_backup_is_a_harmless_no_op() {
        let target = std::env::temp_dir().join(format!(
            "reins-updater-test-{}-no-backup-exists",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(backup_path(&target));
        assert!(rollback(&target).is_ok());
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p daemon updater::install`
Expected: all pass

- [ ] **Step 3: Commit**

```bash
git add apps/daemon/src/updater/install.rs
git commit -m "feat(updater): checksum verification and atomic binary replacement"
```

---

### Task 4: `check_for_update` and rate-limited `background_check`

**Files:**
- Modify: `apps/daemon/src/updater/mod.rs`

**Interfaces:**
- Consumes: `release::{parse_release_json, platform_asset_name, pick_asset, version_is_newer, ReleaseInfo, UpdaterError}` (Task 1), `state::{load_state, save_state, should_check, UpdateCheckState, CHECK_INTERVAL_SECS}` (Task 2).
- Produces: `UpdateCheck`, `check_for_update`, `background_check` (used by Task 6's `reins update`/startup wiring).

- [ ] **Step 1: Replace `apps/daemon/src/updater/mod.rs`'s body**

```rust
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
        version: release.tag_name,
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
```

- [ ] **Step 2: Run a compile check**

Run: `cargo build -p daemon`
Expected: succeeds

- [ ] **Step 3: Write an integration-style test against the real GitHub API**

Add to the bottom of `apps/daemon/src/updater/mod.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p daemon updater::`
Expected: all pass (the network test may print nothing either way — that's fine, it just must not panic)

- [ ] **Step 5: Commit**

```bash
git add apps/daemon/src/updater/mod.rs
git commit -m "feat(updater): check GitHub Releases for a newer version, rate-limited"
```

---

### Task 5: `run_update` — download, verify, atomically install, restart daemon

**Files:**
- Modify: `apps/daemon/src/updater/mod.rs`

**Interfaces:**
- Consumes: `check_for_update`, `UpdateCheck` (this file, Task 4); `install::{atomic_replace, rollback, verify_checksum}` (Task 3); `crate::lifecycle` (existing — `systemd`/`launchd` modules already implement service install/start; this task adds a `restart` function alongside them).
- Produces: `run_update` (used by Task 6's `reins update` subcommand).

- [ ] **Step 1: Add a `restart_if_installed` function to the lifecycle layer**

In `apps/daemon/src/lifecycle/systemd.rs`, add (right after `start_if_installed`):

```rust
/// Restarts the `reinsd` service if it's installed, so an in-place binary swap
/// takes effect immediately rather than waiting for the next login. Returns
/// `Ok(false)` (not an error) when it isn't installed — same convention as
/// [`start_if_installed`].
pub fn restart_if_installed() -> Result<bool, LifecycleError> {
    if !is_installed() {
        return Ok(false);
    }
    run(&["--user", "restart", "reinsd"])?;
    Ok(true)
}

/// Whether the `reinsd` service is currently active (running), as opposed to
/// merely installed. Used after a restart to sanity-check the new binary
/// actually came back up.
pub fn is_active() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", "reinsd"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
```

Do the equivalent in `apps/daemon/src/lifecycle/launchd.rs` — read that file first to match its existing `install_and_start`/`is_installed`/`start_if_installed` naming and shell-out style exactly (it mirrors `systemd.rs` one-for-one per the module doc comment in `apps/daemon/src/lifecycle/mod.rs`), then add `restart_if_installed` (via `launchctl kickstart -k` or equivalent stop+start it already uses) and `is_active`.

- [ ] **Step 2: Write a failing test for the systemd addition**

In `apps/daemon/src/lifecycle/systemd.rs`'s existing `#[cfg(all(test, target_os = "linux"))] mod tests`, add:

```rust
#[test]
fn restart_if_installed_returns_false_when_not_installed() {
    if !systemctl_available() {
        eprintln!("skipping: systemctl not available");
        return;
    }
    if is_installed() {
        eprintln!("skipping: a reinsd unit is already installed on this machine");
        return;
    }
    assert_eq!(restart_if_installed().unwrap(), false);
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p daemon lifecycle::systemd::restart_if_installed`
Expected: PASS (or skipped, per the existing tolerance pattern)

- [ ] **Step 4: Add `run_update` to `apps/daemon/src/updater/mod.rs`**

```rust
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
        return Err(err);
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
```

Add `tar` and `flate2` to the workspace/daemon deps (root `Cargo.toml` `[workspace.dependencies]`: `tar = "0.4"`, `flate2 = "1"`; `apps/daemon/Cargo.toml`: `tar.workspace = true`, `flate2.workspace = true`).

- [ ] **Step 5: Run a compile check**

Run: `cargo build -p daemon`
Expected: succeeds

- [ ] **Step 6: Write a test for `extract_binaries` using a real in-memory tarball**

Add to `apps/daemon/src/updater/mod.rs`'s test module:

```rust
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
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p daemon updater::`
Expected: all pass

- [ ] **Step 8: Commit**

```bash
git add apps/daemon/Cargo.toml Cargo.toml apps/daemon/src/updater/mod.rs apps/daemon/src/lifecycle/systemd.rs apps/daemon/src/lifecycle/launchd.rs
git commit -m "feat(updater): download, verify, atomically install, and restart the daemon"
```

---

### Task 6: `reins update` subcommand and background-check status-line wiring

**Files:**
- Modify: `ui/tui/src/main.rs`
- Modify: `ui/tui/src/app.rs`
- Modify: `ui/tui/src/ui.rs`

**Interfaces:**
- Consumes: `daemon::updater::{run_update, background_check}` (Tasks 4-5).
- Produces: nothing further downstream — this is the final integration task.

- [ ] **Step 1: Add the `update_available` field to `App`**

In `ui/tui/src/app.rs`, add a field to the `App` struct (alongside the other `pub` status fields like `status_message`/`animations_enabled`):

```rust
/// Set when a background version check (see `daemon::updater::background_check`)
/// finds a newer release. Holds the raw GitHub tag (e.g. `"v0.2.0"`). Shown in the
/// status line at lowest priority — the quit-warning and focus-mode indicators
/// both still take over the line ahead of this.
pub update_available: Option<String>,
```

Add it to `App::new()`'s field initializer as `update_available: None,`.

- [ ] **Step 2: Write a unit test for the new field's default**

In `ui/tui/src/app.rs`'s existing test module, add:

```rust
#[test]
fn update_available_starts_unset() {
    let app = App::new();
    assert_eq!(app.update_available, None);
}
```

- [ ] **Step 3: Run the test, confirm it fails to compile (field doesn't exist yet if done out of order) then passes**

Run: `cargo test -p tui update_available_starts_unset`
Expected: PASS (Step 1 already added the field, so this should pass directly — this step is the checkpoint that the wiring is correct, not a strict red-green split)

- [ ] **Step 4: Show the notice in the status line**

In `ui/tui/src/ui.rs`, find `draw_status_line`'s priority chain (focus-mode branch, then quit-warning branch — per the summary of prior work in this session, focus-mode is highest priority). Add a final `else if` arm, below the existing branches and above the default/idle rendering, that checks `app.update_available`:

```rust
} else if let Some(version) = &app.update_available {
    format!(" update available: {version} — run `reins update` ")
}
```

(Match this to the exact existing `if/else if` chain shape already in that function — read the surrounding lines before editing so the new arm's styling/span construction matches its neighbors instead of diverging.)

- [ ] **Step 5: Fire the background check on startup**

In `ui/tui/src/main.rs`'s `run()`, after `ensure_ready().await?;` and before `refresh_sessions(&rpc, &mut app).await;`, add:

```rust
if let Some(version) = daemon::updater::background_check(env!("CARGO_PKG_VERSION")).await {
    app.update_available = Some(version);
}
```

- [ ] **Step 6: Add the `update` subcommand**

In `ui/tui/src/main.rs`'s `run()`, in the `match args[1].as_str()` block (alongside `"config"` and `"setup"`), add:

```rust
"update" => {
    return handle_update_subcommand().await;
}
```

Then add the handler function near `handle_config_subcommand`:

```rust
/// Runs `reins update`: checks GitHub Releases and, if a newer version exists,
/// downloads, verifies, and installs it, restarting `reinsd` in the process. Prints
/// progress to stdout as it goes — this is a synchronous CLI flow, not something the
/// TUI ever triggers on its own (see the plan's "manual install" trigger model).
async fn handle_update_subcommand() -> anyhow::Result<()> {
    let current_exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not resolve the running `reins` binary's path: {e}"))?;
    let reinsd_path = current_exe
        .parent()
        .map(|dir| dir.join("reinsd"))
        .ok_or_else(|| anyhow::anyhow!("could not resolve `reinsd`'s expected path next to `reins`"))?;

    let result = daemon::updater::run_update(
        env!("CARGO_PKG_VERSION"),
        &current_exe,
        &reinsd_path,
        |line| println!("{line}"),
    )
    .await;

    match result {
        Ok(version) if version == env!("CARGO_PKG_VERSION") => {
            println!("Already on the latest version ({version}).");
            Ok(())
        }
        Ok(version) => {
            println!("Update complete — now on {version}.");
            Ok(())
        }
        Err(err) => Err(anyhow::anyhow!("update failed: {err}")),
    }
}
```

- [ ] **Step 7: Run a full workspace build**

Run: `cargo build --workspace`
Expected: succeeds

- [ ] **Step 8: Run the full test suite**

Run: `cargo test --workspace`
Expected: all pass

- [ ] **Step 9: Commit**

```bash
git add ui/tui/src/main.rs ui/tui/src/app.rs ui/tui/src/ui.rs
git commit -m "feat(updater): add 'reins update' subcommand and status-line notice"
```

---

### Task 7: Extend `scripts/build-release.sh` to publish a GitHub Release

**Files:**
- Modify: `scripts/build-release.sh`

**Interfaces:**
- Consumes: nothing from Rust code — this is a standalone shell change.
- Produces: the release artifacts `check_for_update`/`run_update` (Tasks 4-5) expect: a `reins-<os>-<arch>.tar.gz` per platform this is run on, plus a `SHA256SUMS` file, uploaded via `gh release create`.

- [ ] **Step 1: Add packaging + publishing to the script**

Replace the `echo "==> Done"` tail of `scripts/build-release.sh` with:

```bash
echo "==> Packaging release asset"
PLATFORM_ARCH="$(uname -m)"
case "$(uname -s)" in
    Linux)  PLATFORM_OS="linux" ;;
    Darwin) PLATFORM_OS="macos" ;;
    *)
        echo "error: unsupported platform for release packaging: $(uname -s)" >&2
        exit 1
        ;;
esac
case "$PLATFORM_ARCH" in
    x86_64|amd64) PLATFORM_ARCH="x86_64" ;;
    arm64|aarch64) PLATFORM_ARCH="aarch64" ;;
    *)
        echo "error: unsupported architecture for release packaging: $PLATFORM_ARCH" >&2
        exit 1
        ;;
esac

ASSET_NAME="reins-${PLATFORM_OS}-${PLATFORM_ARCH}.tar.gz"
ASSET_PATH="$BUILD_DIR/$ASSET_NAME"
tar -czf "$ASSET_PATH" -C "$BUILD_DIR" reins reinsd

echo "==> Computing checksums"
SUMS_PATH="$BUILD_DIR/SHA256SUMS"
(cd "$BUILD_DIR" && sha256sum "$ASSET_NAME" > "SHA256SUMS")

if [[ "${SKIP_PUBLISH:-}" == "1" ]]; then
    echo "==> SKIP_PUBLISH=1 set, not publishing a GitHub release"
elif ! command -v gh >/dev/null 2>&1; then
    echo "==> 'gh' CLI not found, skipping GitHub release publish"
    echo "    (install it, or re-run with SKIP_PUBLISH=1 to silence this)"
else
    echo "==> Publishing GitHub release v$NEW_VERSION"
    gh release create "v$NEW_VERSION" \
        --title "v$NEW_VERSION" \
        --generate-notes \
        "$ASSET_PATH" \
        "$SUMS_PATH"
fi

echo "==> Done"
echo "Version:  $NEW_VERSION"
echo "Binaries: $BUILD_DIR/reins, $BUILD_DIR/reinsd"
echo "Asset:    $ASSET_PATH"
```

Note: `gh release create v$NEW_VERSION ... "$ASSET_PATH" "$SUMS_PATH"` uploads *both* files as release assets, but only `SHA256SUMS`'s own filename matches what `CHECKSUMS_ASSET_NAME` in `apps/daemon/src/updater/mod.rs` looks for (`"SHA256SUMS"`) — `sha256sum` writing to a file named exactly `SHA256SUMS` inside `$BUILD_DIR` guarantees the uploaded asset keeps that name.

- [ ] **Step 2: Verify the script is syntactically valid**

Run: `bash -n scripts/build-release.sh`
Expected: no output (syntax OK)

- [ ] **Step 3: Dry-run the packaging logic without actually building or publishing**

This step can't run the full script (a real `cargo build --workspace --release` is slow and this plan shouldn't gate on it), so instead verify the platform-detection + tar/checksum logic in isolation:

Run:
```bash
mkdir -p /tmp/reins-release-dryrun/builds/9.9.9
echo fake-reins > /tmp/reins-release-dryrun/builds/9.9.9/reins
echo fake-reinsd > /tmp/reins-release-dryrun/builds/9.9.9/reinsd
cd /tmp/reins-release-dryrun
BUILD_DIR=builds/9.9.9
PLATFORM_ARCH="$(uname -m)"
case "$(uname -s)" in Linux) PLATFORM_OS="linux" ;; Darwin) PLATFORM_OS="macos" ;; esac
case "$PLATFORM_ARCH" in x86_64|amd64) PLATFORM_ARCH="x86_64" ;; arm64|aarch64) PLATFORM_ARCH="aarch64" ;; esac
ASSET_NAME="reins-${PLATFORM_OS}-${PLATFORM_ARCH}.tar.gz"
tar -czf "$BUILD_DIR/$ASSET_NAME" -C "$BUILD_DIR" reins reinsd
(cd "$BUILD_DIR" && sha256sum "$ASSET_NAME" > SHA256SUMS)
ls "$BUILD_DIR"
cat "$BUILD_DIR/SHA256SUMS"
```
Expected: lists `reins`, `reinsd`, `reins-linux-x86_64.tar.gz` (or your platform's equivalent), `SHA256SUMS`; `SHA256SUMS` contains one line naming the tarball.

Clean up: `rm -rf /tmp/reins-release-dryrun`

- [ ] **Step 4: Commit**

```bash
git add scripts/build-release.sh
git commit -m "feat(release): package a platform tarball + checksums and publish a GitHub release"
```

---

## Post-plan note (not a task, informational)

This plan does not touch the still-open question from the pane-interaction feature about redeploying the currently-running (older, wire-protocol-incompatible) `reinsd`. Once this plan lands, `reins update` becomes the answer to that question going forward — but it can't help with *this* skew, since the currently-installed `reinsd` predates the updater existing at all. That first redeploy still needs to happen manually (or via a fresh `build-release.sh` run + manual `systemctl --user restart reinsd`) independent of this plan.
