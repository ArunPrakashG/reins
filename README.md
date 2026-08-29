# Reins

Reins is a meta-harness that lets one person (the "manager") hire,
release, and interrupt a team of AI coding CLI tools ("team members")
from a single place, instead of juggling separate terminals for each
one. Each team member is staffed by a **harness** — the underlying CLI
(Claude Code, Codex CLI, Gemini CLI) — filling a **role** (a
job-title-style label describing what they're doing on the project:
Architect, Implementer, Reviewer, ...). A background daemon
manages session lifecycle and drives each team member as a
real interactive CLI session inside tmux; a terminal UI
lets the manager see the roster, watch a team member's live pane, and
hire/release/interrupt them.

## Building

```bash
cargo build --workspace
```

## Running

Just run `reins`:

```bash
reins
```

The daemon is managed transparently. On first run, `reins` walks you through
a setup wizard that:

1. Checks that `tmux` is installed (required for all sessions)
2. Probes for available harness CLIs (Claude Code, Codex, Gemini)
3. Installs the daemon as an OS-managed service (`systemd --user` on Linux,
   `launchd` agent on macOS) so it persists across logout
4. On Linux, may ask you to run `sudo reins --setup-linger` to enable session
   persistence even when no login session is active

Subsequent runs just attach to the daemon and show the roster.

### Re-detecting Harnesses or Reinstalling the Daemon

Run `reins setup` at any time to re-probe PATH for available harness CLIs,
reinstall the daemon service, and show a status table:

```
tmux:        ✓ 3.7c
claude-code: ✓ available
codex:       ✗ not found
gemini-cli:  ✓ available
daemon:      ✓ running (systemd user service)
```

### Disabling Animations

By default, `reins` plays a splash animation on each launch and animates
status transitions (hiring, running/idle states). To disable animations:

```bash
reins config set animations off
```

To check current settings:

```bash
reins config
```

Settings are stored in `$XDG_CONFIG_HOME/reins/config.toml` (or
`~/.config/reins/config.toml`).

## Paths

Both the daemon and TUI resolve the same paths:

| What | Location |
|---|---|
| Control socket | `$XDG_RUNTIME_DIR/reins/reinsd.sock`, else `~/.local/state/reins/reinsd.sock` (directory forced to mode 0700, socket to 0600) |
| Roster database | `$XDG_DATA_HOME/reins/reins.db`, else `~/.local/share/reins/reins.db` |
| Config file | `$XDG_CONFIG_HOME/reins/config.toml`, else `~/.config/reins/config.toml` |

The roster database stores session **metadata only** — never
conversation content. Because it persists across restarts while tmux
sessions may not, the daemon reconciles the stored roster against tmux at
startup and marks vanished sessions as exited.

## Repository Structure

The workspace is organized as:

- **`apps/`** — primary binaries and core logic
  - `core/` — domain types and traits (harness profiles, session status, etc.)
  - `daemon/` — background daemon binary and service lifecycle management
  - `store/` — metadata-only SQLite store for session roster
- **`packages/`** — shared libraries
  - `proto/` — JSON-RPC protocol types and service path resolution
  - `adapters/` — harness adapter trait, registry, and CLI-specific implementations
- **`ui/`** — user interface
  - `tui/` — terminal UI binary, setup wizard, config file handling, and animations

## Design

The original architecture and session model are documented in
[`docs/superpowers/specs/2026-08-28-reins-design.md`](docs/superpowers/specs/2026-08-28-reins-design.md).

This phase's work on packaging, daemon lifecycle, first-run UX, and single-binary
experience is detailed in
[`docs/superpowers/specs/2026-08-28-reins-packaging-ux-design.md`](docs/superpowers/specs/2026-08-28-reins-packaging-ux-design.md).
