# Reins — Packaging, Daemon Lifecycle & First-Run UX Design

**Date:** 2026-08-28
**Status:** Approved for planning
**Amends:** `2026-08-28-reins-design.md` (the original design). This spec does not
replace it — it restructures the repository, changes how the daemon is
distributed/launched, and adds first-run setup + branding. The original
spec's architecture (Session Manager, Adapter Registry, Conversation access
model, Capability Router, daemon↔UI protocol) is unchanged and still the
authority for those subsystems.

## 1. Summary

The MVP shipped two binaries (`reinsd`, `reins`) that the user had to run and
sequence manually. This is bad UX: the user should never think about the
daemon at all. This spec makes `reins` the only command a user ever types —
it manages `reinsd`'s installation, startup, and lifecycle transparently —
and adds the onboarding (first-run setup wizard, harness/tool detection) and
branding (splash animation, animated in-progress states) needed for a real
product rather than a dev-only MVP. It also restructures the repository into
a clearer monorepo layout now that the crate boundaries from the original
spec have proven out in practice.

## 2. Goals / Non-Goals

**Goals:**
- The user runs `reins` and nothing else. Daemon install/start/health is
  handled underneath.
- First run walks through a short, skippable setup: checks `tmux`, probes
  which harness CLIs are actually installed, installs the daemon as a
  real OS-managed service (systemd user unit on Linux, launchd agent on
  macOS) so it survives logout, not just app-quit.
- Uninstalled harnesses are simply absent from the hire flow, not shown
  as broken options. If literally none are installed, setup stops and
  tells the user what to install.
- A `reins setup` subcommand re-runs detection/install on demand (manual
  re-detection, per the earlier design decision — no automatic polling).
- A brand splash animation (tachyonfx) plays on every launch, and small
  animated indicators reflect in-progress states (hiring, running). Both
  are togglable via a config file.
- Repository reorganized into `apps/` (core, daemon, store),
  `packages/` (proto, adapters), `ui/` (tui) — mechanical restructure of
  the existing crates, no logic changes from the restructure itself.

**Non-goals (this spec):**
- macOS support is in scope for the *daemon service install* (launchd),
  but no other macOS-specific work (e.g. code signing, notarization,
  a `.app` bundle) is covered here.
- No in-TUI settings screen — the config file is hand-edited or set via
  `reins config set`.
- No change to the daemon↔UI protocol, session model, adapter design, or
  conversation-access model from the original spec — those are unchanged.

## 3. Monorepo Layout

```
reins/
├── apps/
│   ├── core/          # domain types + traits (was reins-core) — crate name: core
│   ├── daemon/         # reinsd binary + lib — crate name: daemon
│   │   └── src/
│   │       ├── session_manager.rs, rpc_server.rs, tmux.rs   (unchanged from MVP)
│   │       └── lifecycle/           # NEW: service install/start logic (§5)
│   │           ├── mod.rs
│   │           ├── systemd.rs
│   │           └── launchd.rs
│   └── store/          # metadata-only SQLite store (was reins-store) — crate name: store
├── packages/
│   ├── proto/           # JSON-RPC types + socket/service path resolution — crate name: proto
│   └── adapters/         # crate name: adapters
│       ├── src/
│       │   ├── lib.rs, registry.rs      # HarnessAdapter/AdapterFactory/AdapterRegistry trait surface
│       │   └── impl/
│       │       ├── claude_code.rs
│       │       ├── codex.rs
│       │       └── gemini_cli.rs
│       └── profiles/*.toml
├── ui/
│   └── tui/              # reins binary — crate name: tui
│       └── src/
│           ├── app.rs, client.rs, ui.rs, main.rs   (unchanged from MVP)
│           ├── setup/               # NEW: first-run wizard (§4)
│           ├── effects.rs           # NEW: tachyonfx splash + status animations (§6)
│           └── config.rs            # NEW: config file read/write (§6)
└── docs/superpowers/{specs,plans}/
```

Crate names drop the `reins-` prefix (directory structure now provides the
namespace). Cargo workspace root updates its `members` list to the new
paths. This move is purely mechanical: `git mv` each crate directory,
fix up relative path dependencies in each `Cargo.toml`, fix `include_str!`
paths, re-run the full test suite to confirm nothing broke. No production
logic changes.

## 4. Daemon Lifecycle & Single-Binary UX

**Every `reins` invocation does this preamble before showing the TUI:**

```
reins starts
   │
   ▼
Is the setup-complete marker present?
  ($XDG_STATE_HOME/reins/setup-complete, falling back to
   ~/.local/state/reins/setup-complete)
   │
   ├─ No  → run first-run setup wizard (§5), then continue
   │
   ▼
Is the daemon socket alive? (same liveness check as the MVP TUI already does)
   │
   ├─ No  → is a systemd/launchd service registered for the daemon?
   │           ├─ Yes → start it (`systemctl --user start reinsd` /
   │           │         `launchctl kickstart gui/$UID/dev.reins.daemon`),
   │           │         wait for the socket to come up (bounded retry)
   │           └─ No  → fallback: spawn `reinsd` directly, detached
   │                     (setup should have installed the service, so
   │                      this path is a safety net, not the normal one)
   ▼
Attach as TUI
```

**Service install** (performed once, during first-run setup, §5 step 4):

- **Linux (systemd):** write a `systemd --user` unit to
  `~/.config/systemd/user/reinsd.service` (`ExecStart` = the resolved
  `reinsd` binary path, `Restart=on-failure`), then
  `systemctl --user daemon-reload && systemctl --user enable --now reinsd`.
  This alone is fully unprivileged and starts the daemon on every login.
  To survive a *full* logout (no active session at all, not just no
  terminal open), the wizard also attempts `loginctl enable-linger
  $USER` — this requires elevated privileges. If it fails with a
  permissions error, the wizard prints a clear explanation and exits
  non-zero with the instruction: **`sudo reins --setup-linger`** — a
  small, single-purpose elevated re-invocation that does only the
  `loginctl enable-linger` call and exits, not a full sudo re-run of
  the wizard.
- **macOS (launchd):** write a launchd agent plist to
  `~/Library/LaunchAgents/dev.reins.daemon.plist`
  (`RunAtLoad=true`, `KeepAlive=true`), then
  `launchctl bootstrap gui/$UID ~/Library/LaunchAgents/dev.reins.daemon.plist`.
  launchd user agents already survive logout/login by default — no
  linger-equivalent step, no sudo needed on macOS.

`packages/proto` gains the service-unit-path and socket-path resolution
logic (it already owns `control_socket_path()` from the MVP; this is an
additive sibling, `service_unit_path()`/`plist_path()`), since both
`apps/daemon` (to write the file) and `ui/tui` (to check whether it
exists, for the "is a service registered" branch above) need it.

## 5. First-Run Setup Wizard

Gated by the setup-complete marker (§4). Lives in `ui/tui/src/setup/`.

```
1. Brand splash (tachyonfx, §6) — plays here too, same as every launch.
2. Check tmux — hard requirement.
   Found   → ✓ tmux <version> detected
   Missing → print per-OS install hint (apt/brew/pacman), exit non-zero.
             Nothing in Reins works without tmux; no point continuing.
3. Probe harness CLIs on PATH: claude, codex, gemini (via each adapter's
   `is_available()`, §7).
   Found    → ✓ registered, available for hiring
   Missing  → marked unavailable; silently absent from later hire flows
              (not greyed-out — nothing actionable until the user installs
               it and runs `reins setup` again)
   ALL missing → prompt: "No AI coding CLI found. Install at least one
     (Claude Code, Codex CLI, or Gemini CLI) and run `reins` again."
     Exit non-zero.
4. Install + start the daemon service (§4).
   On the Linux linger-permission-failure path: print the
   `sudo reins --setup-linger` instruction and exit non-zero — setup is
   incomplete until that runs, rather than silently proceeding without
   linger and having a session vanish on logout later without warning.
5. Write the setup-complete marker.
6. Hand off into the normal TUI.
```

`reins setup` (explicit subcommand, run any time) re-runs steps 2-4 on
demand and prints a compact status table instead of the full wizard flow:

```
tmux:        ✓ 3.7c
claude-code: ✓ available
codex:       ✗ not found
gemini-cli:  ✓ available
daemon:      ✓ running (systemd user service)
```

`reins setup` does not require the daemon to be running — it probes `PATH`
directly, so it also works as a pre-flight check before the daemon exists
at all.

## 6. Config File & Animation Toggle

**`$XDG_CONFIG_HOME/reins/config.toml`**, falling back to
`~/.config/reins/config.toml`. Read once at `ui/tui` startup
(`ui/tui/src/config.rs`).

```toml
animations = true
```

- **`reins config set animations <on|off>`** — writes the key, creating
  the file and parent directories if absent.
- **`reins config`** with no args — prints current settings.
- This file is a natural home for future settings; only the one key is
  built now (YAGNI) — no in-TUI settings screen per the earlier decision.

## 7. Harness Availability

`packages/adapters`' `HarnessAdapter` trait gains one method:

```rust
trait HarnessAdapter: Send + Sync {
    // ...existing methods unchanged...
    fn is_available(&self) -> bool {
        which(self.spawn_command_program()) // default: resolve the
                                             // adapter's own binary name
                                             // on PATH; adapters may
                                             // override for a more
                                             // specific check later
    }
}
```

Wiring (no new concept — slots into the existing daemon startup / profile
loading from the MVP):

- The daemon's startup profile-loading step (existing code) filters:
  only profiles whose adapter's `is_available()` is `true` go into the
  `Vec<HarnessProfile>` served to `ListHarnesses`/`ManualRouter`. A
  missing harness just never appears in the hire flow's picker.
- `reins setup`'s status table and the wizard's step 3 are the same
  `is_available()` check, called standalone against each registered
  adapter (no daemon required).

## 8. Branding & Animation (tachyonfx)

- **Splash plays on every `reins` launch** (not just first-run) — a
  static ASCII "REINS" wordmark rendered into a `ratatui::buffer::Buffer`
  region, animated in via a `tachyonfx::Effect` (materializing /
  fading in over roughly 800ms-1.2s) driven by an `EffectManager`
  ticking once per frame. Skippable on any keypress. Skipped entirely
  when `animations = false` (§6) — straight to the daemon-liveness
  check.
- **In-progress state animations**, same `EffectManager` mechanism,
  `ui/tui/src/effects.rs`:
  - **Hiring**: between sending `Request::Hire` and the first successful
    `GetPaneSnapshot` confirming the session is alive, the new roster
    row's status position shows a pulse/shimmer effect instead of a
    static glyph.
  - **Running vs. AwaitingInput**: the existing `●` status glyph
    (added in the MVP's final review fix wave) gets a subtle continuous
    effect — slow pulse for `Running`, steady for `AwaitingInput`.
  - When `animations = false`: both render their static equivalent
    (`●`/`○`, no motion) — same underlying `SessionStatus`, just no
    effect applied.
- Actual ASCII wordmark content (font/style) is an implementation detail
  for the build task, not an architecture decision.

## 9. Coding Standards Addendum

- `apps/daemon/src/lifecycle/{systemd,launchd}.rs` shell out via
  `std::process::Command` (same pattern as `TmuxController`) — no new
  process-management dependency needed.
- Service-file writing uses `std::fs`, no templating engine — the unit
  file / plist content is small enough for a plain `format!()`.
- `ui/tui/src/setup/` and `config.rs` follow the existing crate's
  `thiserror` discipline; `anyhow` stays confined to `main.rs`.
- `tachyonfx` added as a `ui/tui` dependency only — no other crate needs
  it.

## 10. MVP Scope (this phase)

Everything in §3-§9. This phase does not touch §6-§9 of the *original*
design spec (Conversation Access, Capability Router, Orchestrator/Planner,
daemon↔UI protocol) — those remain as previously specified and
implemented. The deferred conversation-access work flagged in the MVP's
final review is still deferred; it is not part of this phase either.
