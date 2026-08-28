//! Workspace-wide end-to-end integration test for `reins-daemon`.
//!
//! Unlike the `#[cfg(test)] mod tests` blocks inside `src/rpc_server.rs` and
//! `src/session_manager.rs` (which link directly into the crate and can see its
//! internals), this file lives in `tests/` — Rust's standard integration-test
//! convention. It is compiled as a wholly separate binary that can only see
//! `reins_daemon`'s public API (`reins_daemon::rpc_server`, `session_manager`, `tmux`),
//! the same surface any external embedder would use.
//!
//! It drives the full `Hire -> ListSessions -> Release` sequence over the real Unix
//! socket, backed by a local fake adapter (spawning `sleep 30` instead of a real
//! Claude Code/Codex/Gemini CLI binary) so the test has no dependency on those tools
//! being installed — consistent with the fake-adapter pattern used by
//! `session_manager`'s own internal tests.

use reins_adapters::{AdapterFactory, AdapterRegistry, HarnessAdapter, SpawnContext, TerminalSnapshot};
use reins_core::{ConversationTurn, HarnessProfile, HarnessStatus};
use reins_daemon::rpc_server::run_control_server;
use reins_daemon::session_manager::SessionManager;
use reins_daemon::tmux::TmuxController;
use reins_proto::{Request, Response, ResponseBody};
use reins_store::SqliteStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Minimal fake harness adapter: spawns `sleep 30` in the tmux session instead of a
/// real AI coding CLI, so this test doesn't depend on Claude Code/Codex/Gemini being
/// installed in the environment running it.
struct FakeAdapter;
struct FakeFactory;

impl AdapterFactory for FakeFactory {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn create(&self, _profile: HarnessProfile) -> Box<dyn HarnessAdapter> {
        Box::new(FakeAdapter)
    }
}

impl HarnessAdapter for FakeAdapter {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn profile(&self) -> &HarnessProfile {
        unimplemented!("not exercised by this test")
    }
    fn spawn_command(&self, _ctx: &SpawnContext) -> std::process::Command {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("30");
        cmd
    }
    fn interrupt_keys(&self) -> &[u8] {
        b"\x03"
    }
    fn detect_status(&self, _screen: &TerminalSnapshot) -> HarnessStatus {
        HarnessStatus::Idle
    }
    fn log_dir(&self, _ctx: &SpawnContext) -> PathBuf {
        PathBuf::from("/tmp")
    }
    fn parse_log(&self, _path: &Path) -> Vec<ConversationTurn> {
        vec![]
    }
}

fn tmux_available() -> bool {
    std::process::Command::new("tmux").arg("-V").output().is_ok()
}

fn sample_profiles() -> Arc<Vec<HarnessProfile>> {
    Arc::new(vec![HarnessProfile {
        id: "fake".into(),
        display_name: "Fake Harness".into(),
        strengths: vec![],
        constraints: vec![],
        notes: String::new(),
    }])
}

async fn send(socket_path: &Path, req: &Request) -> Response {
    let mut stream = UnixStream::connect(socket_path).await.unwrap();
    let mut msg = serde_json::to_string(req).unwrap();
    msg.push('\n');
    stream.write_all(msg.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

/// Full round trip through `reins-daemon`'s public API: start `run_control_server` in
/// process, hire a session backed by the fake adapter, list sessions and confirm it
/// appears, release it, and confirm it's gone.
#[tokio::test]
async fn hire_list_release_round_trip_through_public_api() {
    if !tmux_available() {
        eprintln!("skipping: tmux not installed");
        return;
    }

    let socket_path =
        std::env::temp_dir().join(format!("reins-integration-test-{}.sock", std::process::id()));

    let store = Arc::new(SqliteStore::open_in_memory().expect("open in-memory store"));
    let mut registry = AdapterRegistry::new();
    registry.register(Box::new(FakeFactory));
    let manager = Arc::new(SessionManager::new(registry, TmuxController, store));
    let profiles = sample_profiles();

    let path_clone = socket_path.clone();
    let manager_clone = manager.clone();
    let profiles_clone = profiles.clone();
    let server_task = tokio::spawn(async move {
        run_control_server(&path_clone, manager_clone, profiles_clone).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Hire: fake harness in /tmp.
    let hire_resp = send(
        &socket_path,
        &Request::Hire {
            harness_id: "fake".into(),
            project_path: "/tmp".into(),
            role: Some("Architect".into()),
            brief: None,
        },
    )
    .await;
    let session_id = match hire_resp {
        Response::Ok { result: ResponseBody::Session(session) } => {
            assert_eq!(session.harness_id, "fake");
            assert_eq!(session.role.as_deref(), Some("Architect"));
            session.id
        }
        other => panic!("expected Ok(Session(..)) from Hire, got {other:?}"),
    };

    // ListSessions: confirm the hired session appears.
    let list_resp = send(&socket_path, &Request::ListSessions { project_path: None }).await;
    match list_resp {
        Response::Ok { result: ResponseBody::Sessions(sessions) } => {
            assert!(
                sessions.iter().any(|s| s.id == session_id),
                "expected hired session {session_id} to appear in ListSessions, got {sessions:?}"
            );
        }
        other => panic!("expected Ok(Sessions(..)) from ListSessions, got {other:?}"),
    }

    // Release: confirm it succeeds.
    let release_resp =
        send(&socket_path, &Request::Release { session_id: session_id.clone() }).await;
    assert!(
        matches!(release_resp, Response::Ok { result: ResponseBody::Empty }),
        "expected Ok(Empty) from Release, got {release_resp:?}"
    );

    // ListSessions again: confirm the session no longer shows as active. The store
    // still has the row (status Killed) but list_sessions with no filter returns all
    // sessions regardless of status in this MVP, so assert on status instead of
    // absence — check via a fresh Hire/Release-agnostic sanity: the tmux session
    // itself must be gone.
    let tmux_check = std::process::Command::new("tmux")
        .args(["has-session", "-t", &format!("reins-{session_id}")])
        .output()
        .expect("run tmux has-session");
    assert!(
        !tmux_check.status.success(),
        "expected tmux session for {session_id} to be gone after Release"
    );

    let _ = std::fs::remove_file(&socket_path);
    server_task.abort();
}
