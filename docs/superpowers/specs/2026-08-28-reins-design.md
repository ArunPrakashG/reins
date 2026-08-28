# Reins — Design Spec

**Date:** 2026-08-28
**Status:** Approved for planning

## 1. Summary

Reins is a meta-harness that lets one person ("the manager") run and
supervise a team of AI coding CLI tools ("team members") — Claude
Code, Codex CLI, Gemini CLI, and others — from one place, instead of
juggling separate terminals. Each team member is staffed by a
**harness** (the underlying CLI) filling a **role** (a job-title-style
label describing what they're doing on this project: Architect,
Implementer, Reviewer, Researcher, ...).

Reins does not replace these tools or reimplement their intelligence.
It is a control layer: it launches them as real interactive CLI
sessions (via tmux + PTY), lets the manager see and drive them from a
unified UI, and — in later phases — can decompose a bigger request
into a task graph and staff each task with an appropriate harness.

## 2. Goals / Non-Goals

**Goals:**
- Single daemon (`reinsd`) owns all session lifecycle, tmux/PTY
  control, and harness log parsing — one source of truth regardless
  of how many UI clients are attached.
- Any-language UI clients (Ratatui CLI first, native desktop later)
  attach over a local socket boundary; the daemon has zero
  UI-framework coupling.
- Adding a new harness (a new CLI tool) requires implementing one
  adapter + factory + profile file — no changes to session
  management, storage, or routing.
- No content duplication: each harness's own on-disk session
  transcript is the permanent record of what was said. Reins never
  copies conversation content into its own database.
- MVP ships with **manual** role/harness assignment; the router
  interface is shaped so automatic (capability-based) assignment is
  additive later, not a rewrite.

**Non-goals (for this spec):**
- Reins does not call any LLM API directly. All "intelligence" —
  including future planning/decomposition — is delegated to a
  harness, keeping "harnesses are the workers" strictly true.
- No headless/structured-output mode is used to drive harnesses.
  Sessions run exactly as a human would run them, in a real terminal.
- Automatic capability routing, Wave Planning execution, and review
  gates are designed at the architecture level here (section 8) but
  are a later implementation phase, not MVP.

## 3. Architecture Overview

```
                    ┌─────────────────────────┐
                    │      reinsd (Rust)       │
                    │                          │
   JSON-RPC ◄───────┤  Control API             │
  (Unix socket)      │  Session Manager         │
                    │  Adapter Registry (factory)│
   raw bytes  ◄───────┤  PTY passthrough chan    │
  (per-session        │  Capability Router       │
   socket/channel)    │  Conversation reader     │◄──── reads (never
                    │  Orchestrator/Planner    │      copies) session
                    └───────────┬──────────────┘      log files
                                │ spawns, controls
                    ┌───────────┴──────────────┐
                    │   tmux (one session/hire) │
                    │  ┌────────┐ ┌────────┐   │
                    │  │ Claude │ │ Codex  │...│
                    │  │  Code  │ │  CLI   │   │
                    │  └────────┘ └────────┘   │
                    └───────────────────────────┘

UI clients (Ratatui first, native desktop later) connect over the
control (JSON-RPC) and passthrough (raw byte) channels. Multiple UI
clients may attach concurrently.
```

## 4. Session Manager

Owns the lifecycle of every team member (session):

- **Hire (spawn)**: given a harness id, working directory (project),
  role label, and optional brief (initial prompt), create a tmux
  session (`reins-<uuid>`), launch the harness via the adapter's spawn
  command inside it, record a `Session` row.
- **State tracking**: coarse status (`starting`, `running`,
  `awaiting_input`, `exited`, `killed`) from tmux liveness +
  adapter-refined `detect_status`. Status is advisory, not
  authoritative — read from the rendered screen, best-effort.
- **Control**: send keystrokes (`tmux send-keys`), interrupt
  (adapter-defined interrupt key), release (`tmux kill-session`),
  reattach.
- **Passthrough**: stream raw PTY bytes to any UI client watching that
  session (via `tmux pipe-pane` or direct PTY capture).
- **Multi-project**: every session belongs to a project (a working
  directory root); sessions are listed/grouped by project (the
  roster).

Session state is live/in-memory, mirrored to SQLite for crash
recovery. On daemon restart: for each recorded session, check if its
tmux session still exists; if not, mark it `exited`.

## 5. Adapter Layer

Trait-based pluggability, built via a **factory + registry** pattern
so new harnesses are additive:

```rust
trait HarnessAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn profile(&self) -> &HarnessProfile;
    fn spawn_command(&self, ctx: &SpawnContext) -> Command;
    fn interrupt_keys(&self) -> &[u8];
    fn detect_status(&self, screen: &TerminalSnapshot) -> HarnessStatus;
    fn log_dir(&self, ctx: &SpawnContext) -> PathBuf;
    fn parse_log(&self, path: &Path) -> Vec<ConversationTurn>;
}

trait AdapterFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn create(&self, profile: HarnessProfile) -> Box<dyn HarnessAdapter>;
}

struct AdapterRegistry {
    factories: HashMap<&'static str, Box<dyn AdapterFactory>>,
}
impl AdapterRegistry {
    fn register(&mut self, factory: Box<dyn AdapterFactory>);
    fn build(&self, id: &str, profile: HarnessProfile)
        -> Result<Box<dyn HarnessAdapter>, RegistryError>;
}
```

- `detect_status` parses the rendered terminal screen (via a VT100
  parser, e.g. the `vt100` crate) for adapter-specific cues. Fragile
  by nature — treated as advisory.
- `parse_log` reads each harness's own structured transcript format
  (e.g. Claude Code → `~/.claude/projects/<encoded-path>/<session-id>.jsonl`)
  into a common `ConversationTurn { role, content, tool_calls_json,
  timestamp }` shape. This is the reliable path for conversation
  content.
- `HarnessProfile` is static per-harness data (capabilities,
  strengths, constraints, display name), loaded from a TOML file —
  tunable without recompiling.
- **Log-file correlation**: since harnesses generate their own session
  IDs, the adapter watches `log_dir` for the newest file created after
  spawn time to identify which file belongs to this session.
- Session Manager and Capability Router depend only on `dyn
  HarnessAdapter` / `HarnessProfile`, never on concrete adapter types.

## 6. Conversation Access (no mirroring)

The harness's own log file is the **only** copy of conversation
content. SQLite holds pointers/metadata only:

```sql
projects(id, path, name, created_at)
sessions(id, project_id, harness_id, role, tmux_session_name, status,
         log_file_path, started_at, ended_at)
```

No `turns` table. Reading a conversation = look up `log_file_path`,
call `adapter.parse_log()` live (cheap — these are small JSONL files).
Live tailing for the UI is a read-only file watch, re-parsing only new
lines and pushing turns to attached clients — never written to our DB.

**Search**: on-demand ripgrep across known `log_file_path`s for MVP
(personal/small-team session volumes; always accurate, no cache to go
stale). A disposable `tantivy`/FTS5 index — explicitly rebuildable,
never authoritative — is a future addition only if search latency
becomes a real problem. YAGNI otherwise.

## 7. Capability Router

MVP is **manual assignment**: the manager picks the harness (and types
a role) when hiring. The router is a trait now so automatic assignment
is additive later:

```rust
trait CapabilityRouter: Send + Sync {
    fn suggest(&self, task: &TaskDescription, profiles: &[HarnessProfile])
        -> Vec<RoutingSuggestion>;
}

struct ManualRouter; // MVP: returns all profiles unranked
```

`HarnessProfile` example:
```toml
id = "claude-code"
display_name = "Claude Code"
strengths = ["architecture", "reasoning", "review", "refactoring"]
constraints = ["no-multimodal-image-diff"]
notes = "Best for judgment calls and cross-file reasoning."
```

No routing-history/scoring table in MVP — nothing to audit yet. A
future `RuleBasedRouter` or `DelegatedRouter` (asks a harness to judge
— see section 8) slots in without touching Session Manager or the UI
contract, which already calls `suggest()`.

## 8. Orchestrator / Planner (future phase)

Delegates planning itself to a harness rather than Reins calling an
LLM directly, keeping "harnesses do the thinking" strictly true:

```
Manager request: "Build OAuth authentication"
        │
        ▼
Orchestrator hires a harness session in "planning mode" (prompt
template asking it to decompose into a task graph and write it to
a known file, e.g. .reins/plan.json)
        │
        ▼
Orchestrator watches for that file, parses into:
  TaskGraph { tasks: Vec<Task> }
  Task { id, description, depends_on: Vec<TaskId>, suggested_role }
        │
        ▼
Wave scheduler: a wave = tasks whose dependencies are satisfied.
Per task: CapabilityRouter.suggest() → hire a session (same Session
Manager as any manual hire) → wait for completion → next wave.
        │
        ▼
Review gate (optional): a review task, delegated to a harness,
against a completed wave's output. Failed review re-opens the task.
```

The Orchestrator is a state machine (`TaskGraph` + wave cursor,
persisted for crash resumption) driving the *same* Session
Manager/Adapter/Router path as any manual hire — planning, execution,
and review are all just sessions with different prompts, not
special-cased execution paths.

**Explicitly open, deferred to a future spec:** exact plan-file
schema/prompt template per harness; how "task completion" is detected
(harness exits vs. sentinel file vs. manager confirms in UI).

## 9. UI Client (Ratatui) + Workspace Layout

```
┌─ Team ────────────────┬─ Terminal pane ───────────────────────┐
│ ▾ open-harness          │ (raw PTY bytes for focused team      │
│   ● Architect (claude-code)  #21                               │
│   ● Implementer (codex)      #44                               │
│ ▾ other-project          ├─ Conversation view (toggle) ─────────┤
│   ○ Researcher (gemini-cli) #07                                │
└──────────────────────────┴────────────────────────────────────┘
[h]ire  [r]elease  [i]nterrupt  [/] search  [tab] switch pane
```

- Two render modes: **raw terminal** (exact PTY passthrough) and
  **parsed conversation** (structured turns from `parse_log`),
  toggleable.
- Team member label format: `{role} ({harness_id})`, falling back to
  `{harness_id}` alone if no role was given.
- Hire flow: pick project → pick harness (from `ManualRouter`, i.e.
  currently the full list) → role label (free-typed or picked from
  the harness profile's `strengths`) → optional brief → spawn.
- Reconnect-safe: on launch, lists sessions from the daemon and
  re-attaches to whatever's already running.

**Workspace layout:**
```
reins/
├── reins-core/     # domain types: Session, Task, HarnessProfile, ConversationTurn, traits
├── reins-adapters/ # AdapterRegistry, per-harness factories/adapters, profile TOMLs
├── reins-store/    # metadata-only SQLite (projects/sessions), no content mirroring
├── reins-daemon/   # reinsd binary: JSON-RPC server, session manager, tmux/PTY glue, orchestrator
├── reins-tui/      # reins binary: Ratatui client
└── reins-proto/    # shared JSON-RPC request/response types (serde), used by daemon + tui
```

## 10. Daemon ↔ UI Protocol

Hybrid transport over local Unix sockets:
- **Control plane**: JSON-RPC for hire/release/interrupt/list/query —
  simple, human-debuggable, trivial to implement a client for in any
  language.
- **Passthrough plane**: one raw-byte channel per active session for
  live PTY output — avoids JSON/base64 overhead on the
  performance-sensitive hot path (terminal rendering). UI clients run
  their own VT100 parser (e.g. `vt100` crate, or an equivalent in
  another language later) against these bytes.

## 11. Coding Standards

- `thiserror` for typed per-module errors; `anyhow` only at binary
  entry points. No `unwrap`/`panic!` outside tests.
- Cargo workspace with the crate boundaries in section 9 — enforces
  the "core is a linked-or-daemon library, UI is swappable" goal
  structurally, not by convention.
- Factory + trait-object pattern at genuine extension seams (adapter
  construction, storage backend) — not applied reflexively elsewhere.
  Concrete types internally.
- Unit tests per adapter's `parse_log`/`detect_status` against fixture
  transcripts. Integration tests spin up a real tmux session against a
  fake CLI script (no dependency on real Claude Code/Codex/Gemini CLI
  binaries in CI).

## 12. Terminology (UI copy only — code stays plain)

| Concept | UI term | Internal name |
|---|---|---|
| A running harness instance | Team member | `Session` |
| Create a session | Hire | `spawn` |
| Kill a session | Release | `kill` |
| Initial prompt | Brief | `initial_prompt` |
| A project's session list | Roster | session list |
| Job-title label on a session | Role | `role: String` |

## 13. MVP Scope (this implementation cycle)

Sections 4, 5, 6, 7 (manual only), 9, 10 — i.e. everything except
section 8 (Orchestrator/Planner), which is architecture-only in this
spec and becomes its own future implementation phase.
