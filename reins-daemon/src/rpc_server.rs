use crate::session_manager::SessionManager;
use reins_core::{CapabilityRouter, HarnessProfile, ManualRouter, TaskDescription};
use reins_proto::{Request, Response, ResponseBody};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Starts the JSON-RPC control server, listening on `socket_path` for newline-delimited
/// `reins_proto::Request` JSON messages and replying with newline-delimited
/// `reins_proto::Response` JSON messages. Runs forever (until the process exits or the
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
                        crate::session_manager::SessionManagerError::SessionNotFound(format!(
                            "no profile registered for harness id '{}'",
                            session.harness_id
                        ))
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::TmuxController;
    use reins_adapters::AdapterRegistry;
    use reins_store::SqliteStore;
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
        registry.register(Box::new(reins_adapters::ClaudeCodeAdapterFactory));
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
}
