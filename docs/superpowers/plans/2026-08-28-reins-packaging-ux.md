# Reins Packaging & First-Run UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure the Reins repository into the `apps/`/`packages/`/`ui/` monorepo layout, make `reins` the only command a user ever runs (it installs/starts/manages the `reinsd` daemon as a real OS service transparently), add a first-run setup wizard with tool detection, and add a tachyonfx-driven brand splash + animated in-progress states, both togglable via a config file.

**Architecture:** Mechanical crate relocation first (so everything downstream is built against the final paths), then additive daemon-lifecycle logic (`apps/daemon/src/lifecycle/`) callable both from `ui/tui`'s wizard and standalone, then the wizard/config/animation layers in `ui/tui`. No changes to the existing daemon↔UI protocol, session model, or adapter trait shapes beyond one new default method (`is_available`).

**Tech Stack:** Existing MVP stack (Rust, tokio, ratatui, crossterm, rusqlite, serde/toml) plus `tachyonfx` 0.25 (`ui/tui` only) for animation. Service management via `std::process::Command` shelling out to `systemctl --user` (Linux) / `launchctl` (macOS) — no new process-management dependency.

**Spec:** `docs/superpowers/specs/2026-08-28-reins-packaging-ux-design.md`, which amends `docs/superpowers/specs/2026-08-28-reins-design.md` (original architecture, unchanged for the subsystems this plan doesn't touch).

## Global Constraints

- No `unwrap`/`panic!` outside tests (carried over from the original spec, still binding).
- Typed errors via `thiserror`; `anyhow` only at binary entry points.
- Crate names drop the `reins-` prefix: `core`, `daemon`, `store`, `proto`, `adapters`, `tui` (spec §3).
- `tachyonfx` is a dependency of `ui/tui` only.
- `reins setup` and the wizard's steps 2-3 (tmux/harness detection) must work without the daemon running — they probe `PATH` directly (spec §5, §7).
- Splash animation plays on every launch (not just first-run); both the splash and the two status-glyph effects respect `animations = false` in the config file (spec §6, §8).
- The Linux linger step is a separate, explicit elevated re-invocation (`sudo reins --setup-linger`) — never a full sudo re-run of the wizard (spec §4).

---

### Task 1: Repository restructure to the monorepo layout

**Files:**
- Move: `reins-core/` → `apps/core/`
- Move: `reins-daemon/` → `apps/daemon/`
- Move: `reins-store/` → `apps/store/`
- Move: `reins-proto/` → `packages/proto/`
- Move: `reins-adapters/` → `packages/adapters/`, and within it: `src/claude_code.rs` → `src/impl/claude_code.rs`, `src/codex.rs` → `src/impl/codex.rs`, `src/gemini_cli.rs` → `src/impl/gemini_cli.rs` (leaving `lib.rs`/`registry.rs` at `src/` top level)
- Move: `reins-tui/` → `ui/tui/`
- Modify: root `Cargo.toml` (workspace `members`), every moved crate's `Cargo.toml` (package `name`, relative path deps), `packages/adapters/src/lib.rs` (module declarations for the new `impl/` path), `apps/daemon/src/main.rs` and any `include_str!`/relative-path references to profile TOML files or sibling crates

**Interfaces:**
- Produces: the same public types/traits as before, just under new crate names (`core::Session` instead of `reins_core::Session`, etc.) and new file paths. No signature changes.
- Consumes: nothing new.

- [ ] **Step 1: Move each crate directory**

```bash
mkdir -p apps packages ui
git mv reins-core apps/core
git mv reins-daemon apps/daemon
git mv reins-store apps/store
git mv reins-proto packages/proto
git mv reins-adapters packages/adapters
git mv reins-tui ui/tui
mkdir -p packages/adapters/src/impl
git mv packages/adapters/src/claude_code.rs packages/adapters/src/impl/claude_code.rs
git mv packages/adapters/src/codex.rs packages/adapters/src/impl/codex.rs
git mv packages/adapters/src/gemini_cli.rs packages/adapters/src/impl/gemini_cli.rs
```

- [ ] **Step 2: Update the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = [
    "apps/core",
    "apps/daemon",
    "apps/store",
    "packages/proto",
    "packages/adapters",
    "ui/tui",
]
```

(Keep `[workspace.package]` / `[workspace.dependencies]` unchanged from the MVP.)

- [ ] **Step 3: Rename each package and fix path dependencies**

In each moved crate's `Cargo.toml`: change `name = "reins-core"` → `name = "core"` (and `reins-daemon` → `daemon`, `reins-store` → `store`, `reins-proto` → `proto`, `reins-adapters` → `adapters`, `reins-tui` → `tui`). Update every `path = "../reins-X"` dependency to the new relative path, e.g. `apps/daemon/Cargo.toml`'s `reins-core = { path = "../reins-core" }` becomes `core = { path = "../core" }`. Compute each relative path carefully given the new nesting (e.g. from `apps/daemon` to `packages/adapters` is `../../packages/adapters`, not `../adapters`).

- [ ] **Step 4: Fix all `use`/`extern crate` references to the renamed crates**

Every `use reins_core::...` becomes `use core::...` (note: Rust's `core` crate name will shadow the standard library's `core` — see Step 5), `use reins_adapters::...` becomes `use adapters::...`, etc., across all `src/` files in every crate.

- [ ] **Step 5: Resolve the `core` naming collision with `std`'s `core` crate**

Rust's implicit prelude includes the real `core` crate (the no-std standard library core). A workspace member named `core` will shadow it in any crate that depends on it, which can cause confusing errors in code using `core::` paths from the standard library (rare in this codebase, but check). If this causes real problems, rename the crate to something collision-free instead (e.g. `reins_core` kept as the Cargo package name while the directory is still `apps/core`, or a different short name like `domain`) — use your judgment and report which you chose and why; this is a case where "drop the prefix" (the design decision) may need a narrow exception for this one crate specifically. Do not silently work around compiler errors without noting the choice.

- [ ] **Step 6: Fix `packages/adapters/src/lib.rs`'s module declarations for the new `impl/` subdirectory**

```rust
mod registry;
mod impl_ {
    pub mod claude_code;
    pub mod codex;
    pub mod gemini_cli;
}
pub use impl_::claude_code::ClaudeCodeAdapterFactory;
pub use impl_::codex::CodexAdapterFactory;
pub use impl_::gemini_cli::GeminiCliAdapterFactory;
pub use registry::{AdapterFactory, AdapterRegistry, RegistryError};
// ... rest unchanged
```

(`impl` is a Rust keyword and cannot be a module name directly — using `impl_` as the actual module identifier while the directory is named `impl/` via `#[path = "impl/mod.rs"]` or Rust 2018+ directory-module conventions; verify the exact mechanism compiles — if `mod impl_ { pub mod claude_code; }` doesn't resolve the directory correctly without a `mod.rs`/`impl_.rs` file, add one at `packages/adapters/src/impl_.rs` with just the three `pub mod` lines, or use `#[path = "impl/claude_code.rs"] mod claude_code;` per submodule directly in `lib.rs` — pick whichever compiles cleanly and is least surprising to a future reader, and note your choice in the task report.)

- [ ] **Step 7: Fix `apps/daemon/src/main.rs`'s profile `include_str!` paths**

The MVP used `include_str!("../../reins-adapters/profiles/claude-code.toml")` (or similar, per Task 10's implementation — check the actual current path). Update to the new relative path from `apps/daemon/src/main.rs` to `packages/adapters/profiles/*.toml` (e.g. `../../packages/adapters/profiles/claude-code.toml`).

- [ ] **Step 8: Build and test**

Run: `cargo build --workspace` then `cargo test --workspace`
Expected: builds clean, all 36 existing tests still pass (no behavior change, only paths/names).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "chore: restructure into apps/packages/ui monorepo layout"
```

---

### Task 2: `packages/proto` — service/socket path resolution additions

**Files:**
- Modify: `packages/proto/src/paths.rs` (already has `control_socket_path()`, `ensure_private_dir()` from the MVP)
- Test: inline in `paths.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn systemd_unit_path() -> Result<PathBuf, std::io::Error>` (`~/.config/systemd/user/reinsd.service`), `pub fn launchd_plist_path() -> Result<PathBuf, std::io::Error>` (`~/Library/LaunchAgents/dev.reins.daemon.plist`), `pub fn setup_marker_path() -> Result<PathBuf, std::io::Error>` (`$XDG_STATE_HOME/reins/setup-complete`, falling back to `~/.local/state/reins/setup-complete`), `pub fn config_file_path() -> Result<PathBuf, std::io::Error>` (`$XDG_CONFIG_HOME/reins/config.toml`, falling back to `~/.config/reins/config.toml`). `apps/daemon`'s lifecycle module and `ui/tui`'s setup/config modules depend on these exact function names.

- [ ] **Step 1: Add the four path functions to `packages/proto/src/paths.rs`**

Follow the existing `control_socket_path()`/`ensure_private_dir()` pattern in the file (same style of `env::var_os` lookups and fallback chains). `systemd_unit_path`/`launchd_plist_path` don't need `ensure_private_dir` (those directories are conventionally 0755, not private — only the socket and DB needed hardening). `setup_marker_path`/`config_file_path` should ensure their parent directory exists (create it if missing) since callers will write directly to these paths.

- [ ] **Step 2: Write tests**

Cover: each function returns a path ending in the expected filename; `config_file_path`/`setup_marker_path` actually create their parent directory as a side effect (assert `path.parent().unwrap().exists()` after calling).

- [ ] **Step 3: Run tests**

Run: `cargo test -p proto`
Expected: all pass, including the 4+ new tests.

- [ ] **Step 4: Commit**

```bash
git add packages/proto
git commit -m "feat(proto): add service unit, plist, setup-marker, and config path resolution"
```

---

### Task 3: `packages/adapters` — `is_available()` and daemon-side filtering

**Files:**
- Modify: `packages/adapters/src/lib.rs` (the `HarnessAdapter` trait)
- Modify: `apps/daemon/src/main.rs` (profile-loading step — filter by availability)
- Test: inline in `lib.rs` and per-adapter files

**Interfaces:**
- Consumes: nothing new beyond what adapters already have (`spawn_command`).
- Produces: `HarnessAdapter::is_available(&self) -> bool` (default-implemented). Task 5-8 (wizard, `reins setup`) call this per registered adapter to build the status table.

- [ ] **Step 1: Add `is_available` to the `HarnessAdapter` trait**

```rust
pub trait HarnessAdapter: Send + Sync {
    // ...existing methods...

    /// Best-effort check that this harness's CLI is actually runnable.
    /// Default: resolve the program named in `spawn_command`'s Command
    /// against PATH. Adapters may override for a more specific check.
    fn is_available(&self) -> bool {
        let ctx = SpawnContext {
            project_path: std::env::temp_dir(),
            role: None,
            brief: None,
        };
        let program = self.spawn_command(&ctx).get_program().to_owned();
        which(&program)
    }
}

fn which(program: &std::ffi::OsStr) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| dir.join(program).is_file())
}
```

Note: calling `spawn_command` just to read its program name (not to actually run it) is a minor abuse of that method's intent — if this feels wrong once you see the real adapter code, an alternative is adding a lightweight `fn program_name(&self) -> &'static str` to the trait instead and having `spawn_command` and `is_available` both use it. Use your judgment; report which you chose.

- [ ] **Step 2: Write a test per adapter (or one parameterized test) confirming `is_available` returns `true` for a program guaranteed to exist (e.g. temporarily construct a test adapter whose `spawn_command` uses `"sh"` or `"true"`) and `false` for a nonsense program name**

- [ ] **Step 3: Wire filtering into `apps/daemon/src/main.rs`'s profile loading**

The existing startup code loads all three profiles into a `Vec<HarnessProfile>` unconditionally. After building each adapter via the registry (or by iterating a parallel `(harness_id, profile, adapter)` list), filter to only those where `adapter.is_available()` is `true` before handing the vector to the RPC server / router. Log which harnesses were filtered out (a simple `eprintln!`/`tracing`-free log line is fine, matching the existing style) so a user checking daemon output can see why a harness is missing.

- [ ] **Step 4: Run tests**

Run: `cargo test -p adapters && cargo test -p daemon`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add packages/adapters apps/daemon
git commit -m "feat(adapters): add is_available(), filter daemon profiles by it"
```

---

### Task 4: `apps/daemon` — systemd lifecycle module

**Files:**
- Create: `apps/daemon/src/lifecycle/mod.rs`
- Create: `apps/daemon/src/lifecycle/systemd.rs`
- Modify: `apps/daemon/src/lib.rs` (expose `pub mod lifecycle;`)
- Test: inline, gated on `cfg(target_os = "linux")` and a `systemctl`-availability guard (same pattern as the MVP's `tmux_available()` guard)

**Interfaces:**
- Consumes: `proto::systemd_unit_path()` (Task 2), the resolved path to the `reinsd` binary (`std::env::current_exe()`).
- Produces: `pub fn install_and_start(reinsd_path: &Path) -> Result<(), LifecycleError>` (writes the unit file, `daemon-reload`, `enable --now`), `pub fn is_installed() -> bool` (unit file exists), `pub fn start_if_installed() -> Result<bool, LifecycleError>` (returns whether it was started), `pub fn enable_linger(user: &str) -> Result<(), LifecycleError>`. Task 8 (wizard) and Task 9 (main.rs preamble) call these.

- [ ] **Step 1: Write `LifecycleError`**

```rust
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("command '{0}' failed: {1}")]
    CommandFailed(String, String),
    #[error("permission denied enabling linger — run: sudo reins --setup-linger")]
    LingerPermissionDenied,
}
```

- [ ] **Step 2: Write the unit file template and `install_and_start`**

```rust
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

pub fn install_and_start(reinsd_path: &Path) -> Result<(), LifecycleError> {
    let unit_path = proto::systemd_unit_path()?;
    let content = UNIT_TEMPLATE.replace("{exec_start}", &reinsd_path.display().to_string());
    std::fs::write(&unit_path, content)?;
    run(&["--user", "daemon-reload"])?;
    run(&["--user", "enable", "--now", "reinsd"])?;
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
```

- [ ] **Step 3: Write `is_installed`, `start_if_installed`, `enable_linger`**

```rust
pub fn is_installed() -> bool {
    proto::systemd_unit_path().map(|p| p.exists()).unwrap_or(false)
}

pub fn start_if_installed() -> Result<bool, LifecycleError> {
    if !is_installed() {
        return Ok(false);
    }
    run(&["--user", "start", "reinsd"])?;
    Ok(true)
}

pub fn enable_linger(user: &str) -> Result<(), LifecycleError> {
    let output = std::process::Command::new("loginctl")
        .args(["enable-linger", user])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Permission") || stderr.contains("not privileged") {
            return Err(LifecycleError::LingerPermissionDenied);
        }
        return Err(LifecycleError::CommandFailed("loginctl enable-linger".into(), stderr.to_string()));
    }
    Ok(())
}
```

(Verify the actual `loginctl` stderr wording for a permission failure in your environment before finalizing the substring match — it may differ across systemd versions; if uncertain, treat any non-zero exit as `LingerPermissionDenied` conservatively rather than misclassifying it as a generic `CommandFailed`, since the wizard's messaging differs meaningfully between the two.)

- [ ] **Step 4: Write `apps/daemon/src/lifecycle/mod.rs`**

```rust
#[cfg(target_os = "linux")]
pub mod systemd;
#[cfg(target_os = "macos")]
pub mod launchd;
```

- [ ] **Step 5: Expose from `apps/daemon/src/lib.rs`**

```rust
pub mod lifecycle;
```

- [ ] **Step 6: Write tests (Linux-only, guarded)**

```rust
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn systemctl_available() -> bool {
        std::process::Command::new("systemctl").arg("--version").output().is_ok()
    }

    #[test]
    fn install_and_start_writes_a_real_unit_file() {
        if !systemctl_available() {
            eprintln!("skipping: systemctl not available");
            return;
        }
        // Use a throwaway ExecStart (e.g. `/bin/true`) so this test doesn't
        // actually need a real reinsd binary; install, assert the unit file
        // exists with expected content, then clean up: `systemctl --user
        // disable --now reinsd`, remove the unit file, `daemon-reload`.
    }
}
```

Write the actual test body following this shape — install against a fake `ExecStart`, assert the file's content, then tear down completely (don't leave a `reinsd` unit registered on the CI/dev machine after the test).

- [ ] **Step 7: Run tests**

Run: `cargo test -p daemon lifecycle::`
Expected: pass (or clean skip if `systemctl` isn't available in this environment).

- [ ] **Step 8: Commit**

```bash
git add apps/daemon
git commit -m "feat(daemon): add systemd lifecycle module (install/start/linger)"
```

---

### Task 5: `apps/daemon` — launchd lifecycle module

**Files:**
- Create: `apps/daemon/src/lifecycle/launchd.rs`
- Test: inline, gated on `cfg(target_os = "macos")`

**Interfaces:**
- Same shape as Task 4's `systemd` module: `install_and_start`, `is_installed`, `start_if_installed`. No `enable_linger` equivalent — launchd user agents survive logout by default (spec §4).

- [ ] **Step 1: Write the plist template and `install_and_start`**

```rust
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

pub fn install_and_start(reinsd_path: &Path) -> Result<(), LifecycleError> {
    let plist_path = proto::launchd_plist_path()?;
    let content = PLIST_TEMPLATE.replace("{exec_start}", &reinsd_path.display().to_string());
    std::fs::write(&plist_path, content)?;
    let uid = unsafe { libc_geteuid_or_env_fallback() }; // see Step 2 note
    run(&["bootstrap", &format!("gui/{uid}"), &plist_path.display().to_string()])?;
    Ok(())
}
```

- [ ] **Step 2: Resolve the current user's UID without adding a new dependency**

`launchctl bootstrap gui/<uid> <path>` needs the numeric UID. Rather than pulling in the `libc`/`nix` crate for a single `geteuid()` call, shell out to `id -u` and parse its stdout (`std::process::Command::new("id").arg("-u")`), consistent with this codebase's existing pattern of shelling out for OS facts rather than adding low-level system-call dependencies. Replace the pseudocode `libc_geteuid_or_env_fallback()` placeholder above with this real approach.

- [ ] **Step 3: Write `is_installed`, `start_if_installed`, `run` helper (mirrors `systemd.rs`'s structure)**

```rust
pub fn is_installed() -> bool {
    proto::launchd_plist_path().map(|p| p.exists()).unwrap_or(false)
}

pub fn start_if_installed() -> Result<bool, LifecycleError> {
    if !is_installed() {
        return Ok(false);
    }
    let uid = current_uid()?;
    run(&["kickstart", &format!("gui/{uid}/dev.reins.daemon")])?;
    Ok(true)
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
```

- [ ] **Step 4: Write tests (macOS-only, guarded on `launchctl` availability)**

Same shape as Task 4 Step 6 — install against a throwaway `ProgramArguments` entry, assert file content, tear down (`launchctl bootout gui/<uid>/dev.reins.daemon`, remove the plist).

- [ ] **Step 5: Run tests**

Run: `cargo test -p daemon lifecycle::launchd::`
Expected: pass on macOS, or report that this can only be verified on a macOS machine if the current environment is Linux — write the code correctly per the documented `launchctl` interface even if it can't be executed here, and say so explicitly in the report rather than claiming verification that didn't happen.

- [ ] **Step 6: Commit**

```bash
git add apps/daemon
git commit -m "feat(daemon): add launchd lifecycle module (install/start)"
```

---

### Task 6: `ui/tui` — config file (animations toggle)

**Files:**
- Create: `ui/tui/src/config.rs`
- Modify: `ui/tui/src/main.rs` (add `reins config [get|set]` subcommand dispatch)
- Test: inline in `config.rs`

**Interfaces:**
- Consumes: `proto::config_file_path()` (Task 2).
- Produces: `pub struct Config { pub animations: bool }` with `impl Default` (`animations: true`), `pub fn load() -> Config` (reads the file, falls back to `Default` on any read/parse error — a corrupt config file should never block startup), `pub fn save(config: &Config) -> Result<(), ConfigError>`. Task 9 (main.rs preamble) and Tasks 11-12 (animations) call `config::load()`.

- [ ] **Step 1: Write `Config`, `load`, `save`**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_animations")]
    pub animations: bool,
}

fn default_animations() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self { animations: true }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

pub fn load() -> Config {
    let Ok(path) = proto::config_file_path() else {
        return Config::default();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    toml::from_str(&content).unwrap_or_default()
}

pub fn save(config: &Config) -> Result<(), ConfigError> {
    let path = proto::config_file_path()?;
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    Ok(())
}
```

- [ ] **Step 2: Write tests**

Cover: `load()` on a nonexistent file returns `Config::default()` (animations: true); `save()` then `load()` round-trips a changed value; `load()` on a malformed TOML file falls back to default rather than panicking.

- [ ] **Step 3: Wire the `reins config` subcommand into `ui/tui/src/main.rs`**

Add basic argv handling before the existing TUI-launch path (this codebase doesn't currently use a CLI-parsing crate like `clap` — check whether it should be added now for this and Task 10's `reins setup` subcommand, or whether a minimal hand-rolled `std::env::args()` match is preferable given only two subcommands exist so far; your call, note the choice in your report). Subcommands: `reins config` (print `animations = <value>`), `reins config set animations on|off` (write via `config::save`).

- [ ] **Step 4: Run tests**

Run: `cargo test -p tui config::`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add ui/tui
git commit -m "feat(tui): add config.toml (animations toggle) and reins config subcommand"
```

---

### Task 7: `ui/tui` — setup wizard steps 2-3 (tmux + harness detection)

**Files:**
- Create: `ui/tui/src/setup/mod.rs`
- Create: `ui/tui/src/setup/detect.rs`
- Test: inline in `detect.rs`

**Interfaces:**
- Consumes: `adapters::AdapterRegistry`/`is_available()` (Task 3).
- Produces: `pub struct DetectionReport { pub tmux: Option<String>, pub harnesses: Vec<(String, bool)> }` (harness id, available), `pub fn detect(registry: &AdapterRegistry, profiles: &[HarnessProfile]) -> DetectionReport`. Task 8 (wizard step 4 + marker) and Task 10 (`reins setup`) both call this and render its result.

- [ ] **Step 1: Write `detect_tmux() -> Option<String>`**

```rust
fn detect_tmux() -> Option<String> {
    let output = std::process::Command::new("tmux").arg("-V").output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
```

- [ ] **Step 2: Write `DetectionReport` and `detect`**

```rust
pub struct DetectionReport {
    pub tmux: Option<String>,
    pub harnesses: Vec<(String, bool)>,
}

pub fn detect(registry: &adapters::AdapterRegistry, profiles: &[core::HarnessProfile]) -> DetectionReport {
    let tmux = detect_tmux();
    let harnesses = profiles
        .iter()
        .filter_map(|profile| {
            let adapter = registry.build(&profile.id, profile.clone()).ok()?;
            Some((profile.id.clone(), adapter.is_available()))
        })
        .collect();
    DetectionReport { tmux, harnesses }
}
```

(Note: this builds a throwaway adapter per profile just to call `is_available()` — reasonable given `AdapterRegistry::build` is cheap, but if it turns out adapters do anything expensive in construction, revisit. Not expected to be a problem given the MVP's adapter constructors are just struct literals.)

- [ ] **Step 3: Write `DetectionReport` helper methods for the two exit conditions the wizard/spec need**

```rust
impl DetectionReport {
    pub fn tmux_missing(&self) -> bool {
        self.tmux.is_none()
    }
    pub fn no_harness_available(&self) -> bool {
        self.harnesses.iter().all(|(_, available)| !available)
    }
}
```

- [ ] **Step 4: Write tests**

Since `detect_tmux` shells out to the real `tmux` (confirmed installed in dev/CI per the MVP's precedent), write a test asserting `detect_tmux().is_some()` in an environment where tmux is installed, with a skip-if-missing guard matching the MVP's `tmux_available()` pattern. For `detect`/`no_harness_available`, use a registry with fake adapters (mirroring the MVP's `FakeAdapter` test-double pattern) rather than depending on real harness CLIs being installed in CI.

- [ ] **Step 5: Run tests**

Run: `cargo test -p tui setup::detect::`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add ui/tui
git commit -m "feat(tui): add setup wizard tmux/harness detection"
```

---

### Task 8: `ui/tui` — setup wizard step 4 (daemon install) + marker + `--setup-linger`

**Files:**
- Modify: `ui/tui/src/setup/mod.rs` (the full wizard flow, calling Task 7's `detect` and the daemon crate's `lifecycle` module from Tasks 4-5)
- Modify: `ui/tui/src/main.rs` (dispatch `--setup-linger`, and call the wizard when the marker is absent)
- Modify: `ui/tui/Cargo.toml` (add `daemon = { path = "../../apps/daemon" }` as a dependency, since the wizard calls `daemon::lifecycle::*`)

**Interfaces:**
- Consumes: `daemon::lifecycle::{systemd, launchd}` (Tasks 4-5), `setup::detect` (Task 7), `proto::setup_marker_path()` (Task 2).
- Produces: `pub fn run_wizard() -> Result<(), SetupError>` — the full steps-2-through-6 sequence from spec §5 (splash is Task 11, wired in afterward). Task 9 (main.rs preamble) calls this when the marker is absent.

- [ ] **Step 1: Write `SetupError`**

```rust
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
```

- [ ] **Step 2: Write `run_wizard`**

```rust
pub fn run_wizard(registry: &adapters::AdapterRegistry, profiles: &[core::HarnessProfile]) -> Result<(), SetupError> {
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
```

- [ ] **Step 3: Write `resolve_reinsd_path`, `current_username`, `write_setup_marker`**

```rust
fn resolve_reinsd_path() -> Result<std::path::PathBuf, SetupError> {
    // The reinsd binary should be a sibling of the running `reins` binary
    // once both are installed together (e.g. via `cargo install` or a
    // packaged release placing both in the same bin directory).
    let current = std::env::current_exe()?;
    let candidate = current.with_file_name("reinsd");
    if candidate.exists() {
        return Ok(candidate);
    }
    // Fallback: resolve `reinsd` on PATH.
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path_var)
        .map(|dir| dir.join("reinsd"))
        .find(|p| p.is_file())
        .ok_or_else(|| SetupError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "reinsd binary not found next to reins or on PATH",
        )))
}

fn current_username() -> Result<String, SetupError> {
    let output = std::process::Command::new("id").arg("-un").output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn write_setup_marker() -> Result<(), SetupError> {
    let path = proto::setup_marker_path()?;
    std::fs::write(&path, b"")?;
    Ok(())
}
```

- [ ] **Step 4: Wire `--setup-linger` into `ui/tui/src/main.rs`**

Before any other startup logic, check `std::env::args().nth(1) == Some("--setup-linger")`; if so, call `daemon::lifecycle::systemd::enable_linger(&current_username)`, print success/failure, and exit immediately (never proceed to the TUI). This should work even without the setup marker present, since it's meant to be run standalone after the wizard printed the instruction.

- [ ] **Step 5: Write a test for `resolve_reinsd_path`'s PATH-fallback branch** (using a temp directory added to a test-scoped `PATH` override, or by testing the logic as a pure function taking the search paths as a parameter rather than reading the real environment — refactor for testability if the direct-env-read version is awkward to test)

- [ ] **Step 6: Run tests**

Run: `cargo test -p tui setup::`
Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add ui/tui
git commit -m "feat(tui): add setup wizard daemon install step, marker, --setup-linger"
```

---

### Task 9: `ui/tui` — main.rs preamble (marker check, daemon liveness, service start/spawn fallback)

**Files:**
- Modify: `ui/tui/src/main.rs`

**Interfaces:**
- Consumes: `setup::run_wizard` (Task 8), `daemon::lifecycle::{systemd,launchd}::{is_installed, start_if_installed}` (Tasks 4-5), the existing `RpcClient`/socket-liveness logic from the MVP.
- Produces: the full preamble sequence from spec §4's diagram, run before the existing TUI launch code.

- [ ] **Step 1: Write the preamble function**

```rust
async fn ensure_ready() -> anyhow::Result<()> {
    let marker = proto::setup_marker_path()?;
    if !marker.exists() {
        setup::run_wizard(&registry(), &profiles()?)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    let socket = proto::control_socket_path()?;
    if socket_is_alive(&socket).await {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    let started = daemon::lifecycle::systemd::start_if_installed()?;
    #[cfg(target_os = "macos")]
    let started = daemon::lifecycle::launchd::start_if_installed()?;

    if started {
        wait_for_socket(&socket).await?;
        return Ok(());
    }

    // Fallback: spawn reinsd directly, detached.
    spawn_detached_reinsd()?;
    wait_for_socket(&socket).await?;
    Ok(())
}
```

Reuse whatever socket-liveness check already exists in this file from the MVP (the current TUI already has to detect "daemon unreachable" for its connection-failure error path per Task 11's review) rather than writing a new one — check `ui/tui/src/main.rs` and `client.rs` first for the existing pattern before adding a duplicate.

- [ ] **Step 2: Write `wait_for_socket` (bounded retry, not infinite)**

```rust
async fn wait_for_socket(socket: &std::path::Path) -> anyhow::Result<()> {
    for _ in 0..20 {
        if socket_is_alive(socket).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    anyhow::bail!("daemon did not become ready in time")
}
```

- [ ] **Step 3: Write `spawn_detached_reinsd`**

```rust
fn spawn_detached_reinsd() -> anyhow::Result<()> {
    let reinsd_path = setup::resolve_reinsd_path()?; // may need to make this pub(crate) from Task 8
    std::process::Command::new(reinsd_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}
```

- [ ] **Step 4: Call `ensure_ready().await?` at the very top of `main`'s async body, before any terminal init**

- [ ] **Step 5: Manual/smoke verification**

Since there's no TTY in typical CI to fully drive this interactively, verify what's checkable: delete the setup marker and any installed systemd unit in a scratch `XDG_*` env override, run `reins` non-interactively (e.g. `timeout 3s cargo run -p tui < /dev/null`), and confirm from output/exit behavior that the wizard ran, the service got installed, and no panic occurred — following the same honest-about-limitations approach the MVP's Task 11/12 used for TTY-less verification.

- [ ] **Step 6: Run the full workspace suite**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add ui/tui
git commit -m "feat(tui): wire setup wizard + daemon auto-start into main.rs preamble"
```

---

### Task 10: `ui/tui` — `reins setup` subcommand

**Files:**
- Modify: `ui/tui/src/main.rs` (subcommand dispatch, extending Task 6's `reins config` handling)
- Modify: `ui/tui/src/setup/mod.rs` (a status-table-only entry point, reusing `detect::detect`)

**Interfaces:**
- Consumes: `setup::detect::detect` (Task 7), the daemon `lifecycle` modules' `is_installed`/liveness (Tasks 4-5, 9).
- Produces: `pub fn print_status(registry: &AdapterRegistry, profiles: &[HarnessProfile])` — prints the table from spec §5, and also re-runs steps 2-4 of the wizard (re-detect + re-install/restart the service) per spec §4's "manual re-run" decision.

- [ ] **Step 1: Write `print_status`**

```rust
pub fn print_status(registry: &adapters::AdapterRegistry, profiles: &[core::HarnessProfile]) {
    let report = detect::detect(registry, profiles);
    println!("tmux:        {}", report.tmux.as_deref().map(|v| format!("✓ {v}")).unwrap_or("✗ not found".into()));
    for (id, available) in &report.harnesses {
        println!("{id:<12} {}", if *available { "✓ available" } else { "✗ not found" });
    }
    #[cfg(target_os = "linux")]
    let installed = daemon::lifecycle::systemd::is_installed();
    #[cfg(target_os = "macos")]
    let installed = daemon::lifecycle::launchd::is_installed();
    println!("daemon:      {}", if installed { "✓ installed" } else { "✗ not installed" });
}
```

- [ ] **Step 2: Wire `reins setup` into `main.rs`'s subcommand dispatch (alongside `config` from Task 6)**

`reins setup` re-runs `setup::run_wizard`'s steps 2-4 logic (detection + install/restart — factor `run_wizard` from Task 8 so the "print + gate on hard failures" portion and the "install" portion are separable, since `reins setup` wants both re-detection AND re-install, but should keep running through non-fatal gaps like a still-missing single harness, printing the table either way, rather than exiting on the first `Err` the way first-run setup does) then calls `print_status` for the final table.

- [ ] **Step 3: Run tests**

Run: `cargo test -p tui`
Expected: all pass (this task is thin glue over Tasks 7-9's tested logic; add a focused test only if `print_status`'s formatting has a non-trivial branch worth asserting on).

- [ ] **Step 4: Commit**

```bash
git add ui/tui
git commit -m "feat(tui): add reins setup subcommand for manual re-detection"
```

---

### Task 11: `ui/tui` — tachyonfx splash animation

**Files:**
- Modify: `ui/tui/Cargo.toml` (add `tachyonfx = "0.25"`)
- Create: `ui/tui/src/effects.rs`
- Modify: `ui/tui/src/main.rs` (play the splash before the wizard/roster, respecting `config::load().animations`)

**Interfaces:**
- Consumes: `config::load()` (Task 6).
- Produces: `pub fn play_splash(terminal: &mut Terminal<...>) -> anyhow::Result<()>` — renders the ASCII wordmark with a tachyonfx entrance effect for ~800ms-1.2s, or returns immediately if `!config.animations` or on the first keypress.

- [ ] **Step 1: Confirm the real `tachyonfx` 0.25 API before writing code**

The exact struct/method names for `Effect` construction and the `EffectManager`'s per-frame update call are not fully pinned down from documentation search alone. Before implementing, run `cargo doc --open -p tachyonfx` (or check docs.rs/tachyonfx/0.25) locally to confirm: how to construct an entrance effect (candidates seen in the wild: `tachyonfx::fx::coalesce(...)`, `fx::fade_from_fg(...)`), how to drive it with elapsed time each frame (likely something like `effect.process(elapsed, &mut buffer, area)` or via an `EffectManager::update`/`process_effects` call — confirm the real signature), and how `Duration`/`Interpolation` are constructed. Do not guess at a signature that "looks plausible" — if genuinely blocked on API uncertainty after checking the docs, report BLOCKED with what you found rather than shipping code that doesn't compile against the real crate.

- [ ] **Step 2: Write a simple ASCII wordmark**

A small const string, e.g. a plain block-letter "REINS" (5-7 lines tall, no external font-generation tooling needed — hand-authored ASCII is fine for a first version):

```rust
const WORDMARK: &str = r#"
██████╗ ███████╗██╗███╗   ██╗███████╗
██╔══██╗██╔════╝██║████╗  ██║██╔════╝
██████╔╝█████╗  ██║██╔██╗ ██║███████╗
██╔══██╗██╔══╝  ██║██║╚██╗██║╚════██║
██║  ██║███████╗██║██║ ╚████║███████║
╚═╝  ╚═╝╚══════╝╚═╝╚═╝  ╚═══╝╚══════╝
"#;
```

- [ ] **Step 3: Write `play_splash` using the confirmed real API from Step 1**

Render `WORDMARK` centered in the terminal as a `Paragraph`, apply the entrance effect over the buffer region it occupies, loop rendering frames with increasing elapsed time until the effect reports complete OR any key is pressed (poll with a short timeout each iteration, same `crossterm::event::poll` pattern already used in the main event loop) OR `!config.animations`, in which case skip straight through without entering the render loop at all.

- [ ] **Step 4: Call `play_splash` from `main.rs`, after terminal init, before the wizard/roster**

- [ ] **Step 5: Manual verification note**

Same TTY limitation as Task 9 — confirm it builds and doesn't panic via a non-interactive smoke run; visual confirmation of the animation itself is not verifiable in this environment, say so honestly in the report.

- [ ] **Step 6: Run tests**

Run: `cargo build -p tui` (zero warnings) — this task likely has little to unit-test directly (it's rendering/timing code), so build-clean plus the smoke run is the practical verification bar here; note this in the report rather than inventing a low-value test.

- [ ] **Step 7: Commit**

```bash
git add ui/tui
git commit -m "feat(tui): add tachyonfx splash animation, togglable via config"
```

---

### Task 12: `ui/tui` — status glyph animations

**Files:**
- Modify: `ui/tui/src/effects.rs` (from Task 11)
- Modify: `ui/tui/src/ui.rs` (the roster rendering added in the MVP's final review fix wave — `status_glyph`)
- Modify: `ui/tui/src/app.rs` (track per-session animation timing state, e.g. when a hire was initiated, so the "hiring" pulse knows its own elapsed time)

**Interfaces:**
- Consumes: `SessionStatus` (existing), `config::load().animations` (Task 6), the tachyonfx API confirmed in Task 11.
- Produces: an animated variant of the roster row rendering used when `animations = true` and a session is `Starting` (hiring pulse) or `Running`/`AwaitingInput` (subtle continuous effect); falls back to the existing static `●`/`○` rendering from the MVP when `animations = false`.

- [ ] **Step 1: Add hire-start timestamp tracking to `App`**

The existing `App` (from the MVP) doesn't currently track *when* a session entered its current status — add a small `HashMap<String, std::time::Instant>` (session id → time first observed in `Starting`) populated in `refresh_sessions`, cleared when a session's status moves off `Starting` or it's removed from the roster.

- [ ] **Step 2: Write the effect application in `ui.rs`'s roster rendering**

For each row: if `!config.animations`, use the existing static glyph unchanged. Otherwise, for `Starting` rows use the tracked elapsed time to drive a looping pulse/shimmer effect (looping, since hire duration is unbounded — unlike the splash's one-shot entrance); for `Running` rows, a slow continuous pulse keyed off the frame render time (e.g. `Instant::now()` since app start, modulo a period) rather than needing new per-row state; for `AwaitingInput`, steady (no motion, per spec §8 — "steady for AwaitingInput" means visually distinct from Running's pulse, not literally an effect).

- [ ] **Step 3: Write a test for the non-animation-library logic**

The tachyonfx rendering itself is hard to unit test meaningfully (visual output), but the *decision* of which sessions get which treatment (`Starting` → pulsing, `Running` → pulsing, `AwaitingInput`/terminal → static, everything static when `animations = false`) is pure logic — extract it into a small function (e.g. `fn animation_state_for(status: SessionStatus, animations_enabled: bool) -> AnimationState` returning an enum) and unit test that directly, rather than testing through the rendering path.

- [ ] **Step 4: Run tests**

Run: `cargo test -p tui`
Expected: pass, including the new `animation_state_for` tests.

- [ ] **Step 5: Commit**

```bash
git add ui/tui
git commit -m "feat(tui): add animated status glyphs for hiring/running sessions"
```

---

### Task 13: Integration verification + README update

**Files:**
- Modify: `README.md`
- Modify/Create: `apps/daemon/tests/integration_test.rs` (update crate paths from the restructure if needed — check it still compiles post-Task-1)

**Interfaces:**
- Consumes: everything above.
- Produces: no new public interface — final verification + user-facing docs.

- [ ] **Step 1: Confirm the existing daemon integration test still passes post-restructure**

Run: `cargo test -p daemon --test integration_test`
Expected: pass (Task 1 should have already fixed any path breakage; this is the final confirmation).

- [ ] **Step 2: Update `README.md`**

Replace the MVP's "start `reinsd` then `reins`" instructions with: `reins` is the only command; first run walks through setup (tmux check, harness detection, daemon service install — may print a `sudo reins --setup-linger` instruction on Linux); subsequent runs just work. Document `reins setup` (re-detect/reinstall) and `reins config set animations off` (disable animation). Update the crate-layout description to match the new `apps/`/`packages/`/`ui/` structure. Keep the link to the original design spec, and add a link to this phase's spec (`2026-08-28-reins-packaging-ux-design.md`).

- [ ] **Step 3: Run the full workspace suite one more time**

Run: `cargo build --workspace && cargo test --workspace`
Expected: zero warnings, all tests passing.

- [ ] **Step 4: Commit**

```bash
git add README.md apps/daemon
git commit -m "docs: update README for single-binary UX; confirm integration test post-restructure"
```
