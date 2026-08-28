# Reins

Reins is a meta-harness that lets one person (the "manager") hire,
release, and interrupt a team of AI coding CLI tools ("team members")
from a single place, instead of juggling separate terminals for each
one. Each team member is staffed by a **harness** — the underlying CLI
(Claude Code, Codex CLI, Gemini CLI) — filling a **role** (a
job-title-style label describing what they're doing on the project:
Architect, Implementer, Reviewer, ...). A background daemon
(`reinsd`) owns session lifecycle and drives each team member as a
real interactive CLI session inside tmux; a terminal UI (`reins`)
lets the manager see the roster, watch a team member's live pane, and
hire/release/interrupt them.

## Building

```bash
cargo build --workspace
```

## Running

Start the daemon first, then the TUI in another terminal:

```bash
reinsd
reins
```

`reinsd` listens on a local Unix socket; `reins` connects to it to
list available harnesses, hire/release/interrupt team members, and
view their live tmux panes.

## Design

See [`docs/superpowers/specs/2026-08-28-reins-design.md`](docs/superpowers/specs/2026-08-28-reins-design.md)
for the full design spec.
