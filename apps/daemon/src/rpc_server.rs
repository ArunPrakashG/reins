use crate::session_manager::SessionManager;
use adapters::TerminalSnapshot;
use reins_core::{
    CapabilityRouter, HarnessProfile, HarnessStatus, ManualRouter, SessionStatus, TaskDescription,
};
use proto::{Request, Response, ResponseBody};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Starts the JSON-RPC control server, listening on `socket_path` for newline-delimited
/// `proto::Request` JSON messages and replying with newline-delimited
/// `proto::Response` JSON messages. Runs forever (until the process exits or the
/// task is aborted) — callers that want it to run in the background should
/// `tokio::spawn` this future themselves.
///
/// Returns an error if the control socket cannot be bound (e.g. permission denied,
/// or the parent directory doesn't exist) instead of panicking the caller's task.
pub async fn run_control_server(
    socket_path: &Path,
    manager: Arc<SessionManager>,
    profiles: Arc<Vec<HarnessProfile>>,
) -> std::io::Result<()> {
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    // Defence in depth: the socket's directory is already private (0700, see
    // `proto::paths::control_socket_path`), but the socket file itself is created
    // subject to the process umask, so narrow it explicitly. On Linux, connect(2) checks
    // write permission on the socket inode, so 0600 restricts control-plane access —
    // which can spawn harness processes as the daemon's owner — to that owner.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    }
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        tokio::spawn(handle_connection(stream, manager.clone(), profiles.clone()));
    }
}

/// Resolves a stable project id from a project path string. Uses the canonicalized
/// path when possible (so different relative/symlinked spellings of the same project
/// collapse to the same id); falls back to the raw string if canonicalization fails
/// (e.g. the path doesn't exist on disk yet) since the MVP does not need strict path
/// validation here.
fn resolve_project_id(project_path: &str) -> String {
    std::fs::canonicalize(project_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| project_path.to_string())
}

/// Maps a harness-reported [`HarnessStatus`] onto the roster's [`SessionStatus`].
///
/// `HarnessStatus::Error` returns `None`: there is no `SessionStatus::Error` variant,
/// and inventing one (or forcing the session into some unrelated existing variant)
/// would misreport it — so an erroring harness simply leaves the stored status alone.
fn session_status_for(status: HarnessStatus) -> Option<SessionStatus> {
    match status {
        HarnessStatus::Idle | HarnessStatus::AwaitingInput => Some(SessionStatus::AwaitingInput),
        HarnessStatus::Running => Some(SessionStatus::Running),
        HarnessStatus::Error => None,
    }
}

/// Captures the session's tmux pane and, along the way, refreshes the roster's idea of
/// the session's status — this is the only place in the daemon that runs an adapter's
/// `detect_status`, and the reason a session ever moves past `Starting`.
///
/// If the tmux session has disappeared, the roster row is marked `Exited` and the
/// capture reports that rather than surfacing a raw tmux failure.
fn capture_pane_and_sync_status(
    manager: &SessionManager,
    profiles: &[HarnessProfile],
    session: &reins_core::Session,
) -> Result<crate::tmux::PaneCapture, crate::session_manager::SessionManagerError> {
    if !manager.session_alive(&session.tmux_session_name) {
        manager.sync_status(&session.id, session.status, SessionStatus::Exited)?;
        return Err(crate::session_manager::SessionManagerError::SessionGone(session.id.clone()));
    }

    // Status detection matches on plain content, so it uses its own plain capture
    // rather than the live/colored one this function returns to the caller — two tmux
    // calls per poll, but keeps the detector's string matching unaffected by escape
    // codes.
    if let Some(profile) = profiles.iter().find(|p| p.id == session.harness_id).cloned() {
        let plain_text = manager.capture_pane(&session.tmux_session_name)?;
        let adapter = manager.adapter_for(&session.harness_id, profile)?;
        let detected = adapter.detect_status(&TerminalSnapshot { text: plain_text });
        if let Some(new_status) = session_status_for(detected) {
            manager.sync_status(&session.id, session.status, new_status)?;
        }
    }

    manager.capture_pane_live(&session.tmux_session_name)
}

async fn handle_connection(
    stream: UnixStream,
    manager: Arc<SessionManager>,
    profiles: Arc<Vec<HarnessProfile>>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => handle_request(request, &manager, &profiles),
        Err(e) => Response::Err { message: e.to_string() },
    };
    let mut out = serde_json::to_string(&response).unwrap_or_else(|e| {
        // Response/ResponseBody are plain derived-serde types over already-serializable
        // fields, so this should be unreachable in practice; fall back to a hand-built
        // error line rather than panicking the connection task if it ever isn't.
        format!(
            "{{\"status\":\"Err\",\"message\":\"failed to serialize response: {}\"}}",
            e.to_string().replace('"', "'")
        )
    });
    out.push('\n');
    let _ = write_half.write_all(out.as_bytes()).await;
}

fn handle_request(
    request: Request,
    manager: &SessionManager,
    profiles: &[HarnessProfile],
) -> Response {
    match request {
        Request::Hire { harness_id, project_path, role, brief } => {
            let Some(profile) = profiles.iter().find(|p| p.id == harness_id) else {
                return Response::Err {
                    message: format!("unknown harness id '{harness_id}'"),
                };
            };
            let project_id = resolve_project_id(&project_path);
            let path = Path::new(&project_path);
            match manager.hire(&harness_id, profile.clone(), &project_id, path, role, brief) {
                Ok(session) => Response::Ok { result: ResponseBody::Session(session) },
                Err(e) => Response::Err { message: e.to_string() },
            }
        }
        Request::Release { session_id } => {
            let result = manager
                .find_session(&session_id)
                .and_then(|session| manager.release(&session.tmux_session_name, &session.id));
            match result {
                Ok(()) => Response::Ok { result: ResponseBody::Empty },
                Err(e) => Response::Err { message: e.to_string() },
            }
        }
        Request::Interrupt { session_id } => {
            let result = manager.find_session(&session_id).and_then(|session| {
                let profile = profiles
                    .iter()
                    .find(|p| p.id == session.harness_id)
                    .cloned()
                    .ok_or_else(|| {
                        crate::session_manager::SessionManagerError::UnknownProfile(
                            session.harness_id.clone(),
                        )
                    })?;
                let adapter = manager.adapter_for(&session.harness_id, profile)?;
                manager.interrupt(&session.tmux_session_name, adapter.interrupt_keys())
            });
            match result {
                Ok(()) => Response::Ok { result: ResponseBody::Empty },
                Err(e) => Response::Err { message: e.to_string() },
            }
        }
        Request::ListSessions { project_path } => {
            let project_id = project_path.map(|p| resolve_project_id(&p));
            match manager.list_sessions(project_id.as_deref()) {
                Ok(sessions) => Response::Ok { result: ResponseBody::Sessions(sessions) },
                Err(e) => Response::Err { message: e.to_string() },
            }
        }
        Request::ListHarnesses => {
            let router = ManualRouter;
            let suggestions = router.suggest(&TaskDescription(String::new()), profiles);
            let harnesses = suggestions
                .into_iter()
                .map(|suggestion| suggestion.profile)
                .collect();
            Response::Ok {
                result: ResponseBody::Harnesses(harnesses),
            }
        },
        Request::GetPaneSnapshot { session_id } => {
            // On-demand passthrough: capture the tmux pane directly per request rather
            // than maintaining a background poller + watch channel (the plan's original
            // sketch). The TUI polls this endpoint itself on a timer, so a background
            // task would duplicate that polling with extra state to manage for no
            // practical behavioral difference. A future streaming upgrade (a persistent
            // per-session byte stream) would replace this on-demand capture.
            let result = manager
                .find_session(&session_id)
                .and_then(|session| capture_pane_and_sync_status(manager, profiles, &session));
            match result {
                Ok(capture) => Response::Ok {
                    result: ResponseBody::PaneSnapshot {
                        text: capture.text,
                        cursor: (capture.cursor_x, capture.cursor_y),
                    },
                },
                Err(e) => Response::Err { message: e.to_string() },
            }
        }
        Request::SendKeys { session_id, input } => {
            let result = manager
                .find_session(&session_id)
                .and_then(|session| manager.send_key_input(&session.tmux_session_name, &input));
            match result {
                Ok(()) => Response::Ok { result: ResponseBody::Empty },
                Err(e) => Response::Err { message: e.to_string() },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::TmuxController;
    use adapters::AdapterRegistry;
    use store::SqliteStore;
    use tokio::io::AsyncBufReadExt;

    fn sample_profiles() -> Arc<Vec<HarnessProfile>> {
        Arc::new(vec![HarnessProfile {
            id: "claude-code".into(),
            display_name: "Claude Code".into(),
            strengths: vec![],
            constraints: vec![],
            notes: String::new(),
        }])
    }

    #[test]
    fn harness_status_maps_onto_session_status() {
        assert_eq!(
            session_status_for(HarnessStatus::Running),
            Some(SessionStatus::Running)
        );
        assert_eq!(
            session_status_for(HarnessStatus::Idle),
            Some(SessionStatus::AwaitingInput)
        );
        assert_eq!(
            session_status_for(HarnessStatus::AwaitingInput),
            Some(SessionStatus::AwaitingInput)
        );
        // No SessionStatus::Error exists — an erroring harness leaves the roster alone.
        assert_eq!(session_status_for(HarnessStatus::Error), None);
    }

    #[test]
    fn unknown_profile_error_renders_a_readable_message() {
        let err = crate::session_manager::SessionManagerError::UnknownProfile("codex".into());
        assert_eq!(err.to_string(), "no profile registered for harness id 'codex'");
    }

    #[tokio::test]
    async fn list_harnesses_round_trips_over_socket() {
        let socket_path = std::env::temp_dir().join(format!("reins-test-{}.sock", std::process::id()));
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let manager = Arc::new(SessionManager::new(AdapterRegistry::new(), TmuxController, store));
        let profiles = sample_profiles();

        let path_clone = socket_path.clone();
        let manager_clone = manager.clone();
        let profiles_clone = profiles.clone();
        tokio::spawn(async move {
            run_control_server(&path_clone, manager_clone, profiles_clone).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut stream = UnixStream::connect(&socket_path).await.unwrap();
        let req = Request::ListHarnesses;
        let mut msg = serde_json::to_string(&req).unwrap();
        msg.push('\n');
        stream.write_all(msg.as_bytes()).await.unwrap();

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        match resp {
            Response::Ok { result: ResponseBody::Harnesses(harnesses) } => {
                assert_eq!(harnesses.len(), 1);
                assert_eq!(harnesses[0].id, "claude-code");
            }
            other => panic!("expected Ok(Harnesses(..)), got {other:?}"),
        }

        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn hire_release_and_list_sessions_round_trip_over_socket() {
        if std::process::Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let socket_path = std::env::temp_dir().join(format!("reins-test2-{}.sock", std::process::id()));
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut registry = AdapterRegistry::new();
        registry.register(Box::new(adapters::ClaudeCodeAdapterFactory));
        let manager = Arc::new(SessionManager::new(registry, TmuxController, store));
        let profiles = sample_profiles();

        let path_clone = socket_path.clone();
        let manager_clone = manager.clone();
        let profiles_clone = profiles.clone();
        tokio::spawn(async move {
            run_control_server(&path_clone, manager_clone, profiles_clone).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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

        let hire_resp = send(
            &socket_path,
            &Request::Hire {
                harness_id: "claude-code".into(),
                project_path: "/tmp".into(),
                role: Some("Architect".into()),
                brief: None,
            },
        )
        .await;
        let session_id = match hire_resp {
            Response::Ok { result: ResponseBody::Session(session) } => session.id,
            other => panic!("expected Ok(Session(..)), got {other:?}"),
        };

        let list_resp = send(&socket_path, &Request::ListSessions { project_path: None }).await;
        match list_resp {
            Response::Ok { result: ResponseBody::Sessions(sessions) } => {
                assert!(sessions.iter().any(|s| s.id == session_id));
            }
            other => panic!("expected Ok(Sessions(..)), got {other:?}"),
        }

        let release_resp = send(&socket_path, &Request::Release { session_id: session_id.clone() }).await;
        assert!(matches!(release_resp, Response::Ok { result: ResponseBody::Empty }));

        let unknown_resp = send(&socket_path, &Request::Interrupt { session_id: "does-not-exist".into() }).await;
        assert!(matches!(unknown_resp, Response::Err { .. }));

        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn get_pane_snapshot_returns_captured_tmux_pane_text() {
        if std::process::Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let socket_path = std::env::temp_dir().join(format!("reins-test3-{}.sock", std::process::id()));
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut registry = AdapterRegistry::new();
        registry.register(Box::new(adapters::ClaudeCodeAdapterFactory));
        let manager = Arc::new(SessionManager::new(registry, TmuxController, store));
        let profiles = sample_profiles();

        let path_clone = socket_path.clone();
        let manager_clone = manager.clone();
        let profiles_clone = profiles.clone();
        tokio::spawn(async move {
            run_control_server(&path_clone, manager_clone, profiles_clone).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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

        let hire_resp = send(
            &socket_path,
            &Request::Hire {
                harness_id: "claude-code".into(),
                project_path: "/tmp".into(),
                role: Some("Architect".into()),
                brief: None,
            },
        )
        .await;
        let session_id = match hire_resp {
            Response::Ok { result: ResponseBody::Session(session) } => session.id,
            other => panic!("expected Ok(Session(..)), got {other:?}"),
        };

        // Give tmux a moment to actually spawn the shell/command before capturing —
        // an empty pane immediately after `new-session -d` is still a valid capture,
        // but this makes the test meaningfully exercise the passthrough end-to-end.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let snapshot_resp =
            send(&socket_path, &Request::GetPaneSnapshot { session_id: session_id.clone() }).await;
        match snapshot_resp {
            Response::Ok { result: ResponseBody::PaneSnapshot { text, cursor: _ } } => {
                // tmux pads captured panes to the terminal's dimensions, so even a
                // blank shell prompt produces multiple lines of content.
                assert!(!text.is_empty(), "expected non-empty pane snapshot text");
            }
            other => panic!("expected Ok(PaneSnapshot(..)), got {other:?}"),
        }

        // The snapshot handler is also what drives status detection: the session must
        // have moved off `Starting` once a pane has been captured and handed to the
        // adapter's `detect_status`.
        let list_resp = send(&socket_path, &Request::ListSessions { project_path: None }).await;
        match list_resp {
            Response::Ok { result: ResponseBody::Sessions(sessions) } => {
                let session = sessions
                    .iter()
                    .find(|s| s.id == session_id)
                    .expect("hired session should be listed");
                assert_ne!(
                    session.status,
                    reins_core::SessionStatus::Starting,
                    "GetPaneSnapshot should have refreshed the session's status"
                );
            }
            other => panic!("expected Ok(Sessions(..)), got {other:?}"),
        }

        let unknown_snapshot =
            send(&socket_path, &Request::GetPaneSnapshot { session_id: "does-not-exist".into() }).await;
        assert!(matches!(unknown_snapshot, Response::Err { .. }));

        // Clean up: release the session so no `reins-*` tmux session is left behind.
        let release_resp = send(&socket_path, &Request::Release { session_id: session_id.clone() }).await;
        assert!(matches!(release_resp, Response::Ok { result: ResponseBody::Empty }));

        let _ = std::fs::remove_file(&socket_path);
    }

    struct FakeAdapter;
    struct FakeFactory;
    impl adapters::AdapterFactory for FakeFactory {
        fn id(&self) -> &'static str { "fake" }
        fn create(&self, _profile: HarnessProfile) -> Box<dyn adapters::HarnessAdapter> {
            Box::new(FakeAdapter)
        }
    }
    impl adapters::HarnessAdapter for FakeAdapter {
        fn id(&self) -> &'static str { "fake" }
        fn profile(&self) -> &HarnessProfile { unimplemented!() }
        fn program_name(&self) -> &'static str { "cat" }
        fn spawn_command(&self, _ctx: &adapters::SpawnContext) -> std::process::Command {
            std::process::Command::new("cat")
        }
        fn interrupt_keys(&self) -> &[u8] { b"\x03" }
        fn detect_status(&self, _s: &TerminalSnapshot) -> HarnessStatus { HarnessStatus::Idle }
        fn log_dir(&self, _ctx: &adapters::SpawnContext) -> std::path::PathBuf { std::path::PathBuf::from("/tmp") }
        fn parse_log(&self, _path: &std::path::Path) -> Vec<reins_core::ConversationTurn> { vec![] }
    }

    #[tokio::test]
    async fn send_keys_round_trips_literal_text_into_a_real_pane() {
        if std::process::Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let socket_path = std::env::temp_dir().join(format!("reins-test4-{}.sock", std::process::id()));
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut registry = AdapterRegistry::new();
        registry.register(Box::new(FakeFactory));
        let manager = Arc::new(SessionManager::new(registry, TmuxController, store));
        // `cat` (via FakeAdapter) rather than the real claude-code binary, so the pane's
        // output is exactly and only what we send it — deterministic to assert on.
        let profiles = Arc::new(vec![HarnessProfile {
            id: "fake".into(),
            display_name: "Fake".into(),
            strengths: vec![],
            constraints: vec![],
            notes: String::new(),
        }]);

        let path_clone = socket_path.clone();
        let manager_clone = manager.clone();
        let profiles_clone = profiles.clone();
        tokio::spawn(async move {
            run_control_server(&path_clone, manager_clone, profiles_clone).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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

        let hire_resp = send(
            &socket_path,
            &Request::Hire {
                harness_id: "fake".into(),
                project_path: "/tmp".into(),
                role: None,
                brief: None,
            },
        )
        .await;
        let session_id = match hire_resp {
            Response::Ok { result: ResponseBody::Session(session) } => session.id,
            other => panic!("expected Ok(Session(..)), got {other:?}"),
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let literal_resp = send(
            &socket_path,
            &Request::SendKeys {
                session_id: session_id.clone(),
                input: proto::KeyInput::Literal { text: "hello reins".into() },
            },
        )
        .await;
        assert!(matches!(literal_resp, Response::Ok { result: ResponseBody::Empty }));

        let enter_resp = send(
            &socket_path,
            &Request::SendKeys {
                session_id: session_id.clone(),
                input: proto::KeyInput::Named { token: "Enter".into() },
            },
        )
        .await;
        assert!(matches!(enter_resp, Response::Ok { result: ResponseBody::Empty }));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let snapshot_resp =
            send(&socket_path, &Request::GetPaneSnapshot { session_id: session_id.clone() }).await;
        match snapshot_resp {
            Response::Ok { result: ResponseBody::PaneSnapshot { text, .. } } => {
                assert!(text.contains("hello reins"), "captured pane: {text:?}");
            }
            other => panic!("expected Ok(PaneSnapshot(..)), got {other:?}"),
        }

        let unknown_resp = send(
            &socket_path,
            &Request::SendKeys {
                session_id: "does-not-exist".into(),
                input: proto::KeyInput::Literal { text: "x".into() },
            },
        )
        .await;
        assert!(matches!(unknown_resp, Response::Err { .. }));

        let release_resp = send(&socket_path, &Request::Release { session_id: session_id.clone() }).await;
        assert!(matches!(release_resp, Response::Ok { result: ResponseBody::Empty }));

        let _ = std::fs::remove_file(&socket_path);
    }
}
