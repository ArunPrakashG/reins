use crate::tmux::TmuxController;
use reins_adapters::{AdapterRegistry, HarnessAdapter};
use reins_core::{Session, SessionStatus};
use reins_store::ConversationStore;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum SessionManagerError {
    #[error("adapter registry error: {0}")]
    Registry(#[from] reins_adapters::RegistryError),
    #[error("tmux error: {0}")]
    Tmux(#[from] crate::tmux::TmuxError),
    #[error("store error: {0}")]
    Store(#[from] reins_store::StoreError),
    #[error("no session found with id '{0}'")]
    SessionNotFound(String),
    #[error("no profile registered for harness id '{0}'")]
    UnknownProfile(String),
    #[error("session '{0}' is no longer running in tmux")]
    SessionGone(String),
}

pub struct SessionManager {
    registry: AdapterRegistry,
    tmux: TmuxController,
    store: Arc<dyn ConversationStore>,
}

impl SessionManager {
    pub fn new(registry: AdapterRegistry, tmux: TmuxController, store: Arc<dyn ConversationStore>) -> Self {
        Self { registry, tmux, store }
    }

    pub fn hire(
        &self,
        harness_id: &str,
        profile: reins_core::HarnessProfile,
        project_id: &str,
        project_path: &Path,
        role: Option<String>,
        brief: Option<String>,
    ) -> Result<Session, SessionManagerError> {
        let adapter = self.registry.build(harness_id, profile)?;
        let ctx = reins_adapters::SpawnContext {
            project_path: project_path.to_path_buf(),
            role: role.clone(),
            brief,
        };
        let session_id = uuid_v4();
        let tmux_name = format!("reins-{session_id}");
        let command = adapter.spawn_command(&ctx);
        self.tmux.new_session(&tmux_name, project_path, command)?;

        let session = Session {
            id: session_id,
            project_id: project_id.to_string(),
            harness_id: harness_id.to_string(),
            role,
            tmux_session_name: tmux_name,
            status: SessionStatus::Starting,
            log_file_path: None,
            started_at: now_ts(),
            ended_at: None,
        };
        self.store.ensure_project(project_id, &project_path.to_string_lossy())?;
        self.store.insert_session(&session)?;
        Ok(session)
    }

    pub fn release(&self, tmux_session_name: &str, session_id: &str) -> Result<(), SessionManagerError> {
        self.tmux.kill_session(tmux_session_name)?;
        self.store.update_status(session_id, SessionStatus::Killed)?;
        Ok(())
    }

    pub fn interrupt(&self, tmux_session_name: &str, interrupt_keys: &[u8]) -> Result<(), SessionManagerError> {
        self.tmux.send_keys(tmux_session_name, interrupt_keys)?;
        Ok(())
    }

    /// Lists sessions from the store, optionally filtered by project id.
    pub fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<Session>, SessionManagerError> {
        Ok(self.store.list_sessions(project_id)?)
    }

    /// Captures the current tmux pane text for a session. On-demand passthrough for
    /// MVP: called fresh per request (see `Request::GetPaneSnapshot`) rather than
    /// backed by a background poller.
    pub fn capture_pane(&self, tmux_session_name: &str) -> Result<String, SessionManagerError> {
        Ok(self.tmux.capture_pane(tmux_session_name)?)
    }

    /// Finds a single session by its id, via an indexed primary-key lookup in the store
    /// (this is on the hot path: the TUI polls `GetPaneSnapshot` every 250ms per client,
    /// and every Release/Interrupt goes through here too).
    pub fn find_session(&self, session_id: &str) -> Result<Session, SessionManagerError> {
        self.store
            .get_session(session_id)?
            .ok_or_else(|| SessionManagerError::SessionNotFound(session_id.to_string()))
    }

    /// Whether the session's backing tmux session still exists.
    pub fn session_alive(&self, tmux_session_name: &str) -> bool {
        self.tmux.session_exists(tmux_session_name)
    }

    /// Records a new status for a session, but only when it actually differs from
    /// `current` — avoiding a store write on every poll tick.
    pub fn sync_status(
        &self,
        session_id: &str,
        current: SessionStatus,
        new_status: SessionStatus,
    ) -> Result<(), SessionManagerError> {
        if current != new_status {
            self.store.update_status(session_id, new_status)?;
        }
        Ok(())
    }

    /// Startup reconciliation: tmux sessions outlive the daemon, but a daemon restart
    /// used to leave the roster claiming sessions were alive when their tmux session had
    /// gone away in the meantime. Marks every not-already-terminal session whose tmux
    /// session no longer exists as [`SessionStatus::Exited`]. Returns how many rows were
    /// updated.
    pub fn reconcile_with_tmux(&self) -> Result<usize, SessionManagerError> {
        let mut reconciled = 0;
        for session in self.store.list_sessions(None)? {
            if matches!(session.status, SessionStatus::Exited | SessionStatus::Killed) {
                continue;
            }
            if !self.tmux.session_exists(&session.tmux_session_name) {
                self.store.update_status(&session.id, SessionStatus::Exited)?;
                reconciled += 1;
            }
        }
        Ok(reconciled)
    }

    /// Builds a harness adapter instance for the given harness id + profile, via the
    /// registered adapter factory. Exposed so callers (e.g. the RPC server) can resolve
    /// harness-specific behavior (like interrupt key sequences) without needing direct
    /// access to the registry.
    pub fn adapter_for(
        &self,
        harness_id: &str,
        profile: reins_core::HarnessProfile,
    ) -> Result<Box<dyn HarnessAdapter>, SessionManagerError> {
        Ok(self.registry.build(harness_id, profile)?)
    }
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn uuid_v4() -> String {
    // Minimal dependency-free v4-ish id: good enough as a tmux session
    // suffix and store primary key. Not cryptographically strong.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use reins_adapters::{AdapterFactory, HarnessAdapter, SpawnContext, TerminalSnapshot};
    use reins_core::{ConversationTurn, HarnessStatus};
    use reins_store::SqliteStore;
    use std::path::PathBuf;

    struct FakeAdapter;
    struct FakeFactory;
    impl AdapterFactory for FakeFactory {
        fn id(&self) -> &'static str { "fake" }
        fn create(&self, _profile: reins_core::HarnessProfile) -> Box<dyn HarnessAdapter> {
            Box::new(FakeAdapter)
        }
    }
    impl HarnessAdapter for FakeAdapter {
        fn id(&self) -> &'static str { "fake" }
        fn profile(&self) -> &reins_core::HarnessProfile { unimplemented!() }
        fn spawn_command(&self, _ctx: &SpawnContext) -> std::process::Command {
            let mut cmd = std::process::Command::new("sleep");
            cmd.arg("30");
            cmd
        }
        fn interrupt_keys(&self) -> &[u8] { b"\x03" }
        fn detect_status(&self, _s: &TerminalSnapshot) -> HarnessStatus { HarnessStatus::Idle }
        fn log_dir(&self, _ctx: &SpawnContext) -> PathBuf { PathBuf::from("/tmp") }
        fn parse_log(&self, _path: &std::path::Path) -> Vec<ConversationTurn> { vec![] }
    }

    fn tmux_available() -> bool {
        std::process::Command::new("tmux").arg("-V").output().is_ok()
    }

    #[test]
    fn reconcile_marks_sessions_without_a_tmux_session_as_exited() {
        if !tmux_available() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        store.conn_for_test_insert_project("p1");
        // A session recorded as live whose tmux session does not exist — exactly the
        // state a daemon restart leaves behind when the harness died while it was down.
        let stale = Session {
            id: "stale".into(),
            project_id: "p1".into(),
            harness_id: "fake".into(),
            role: None,
            tmux_session_name: "reins-definitely-not-a-real-tmux-session".into(),
            status: SessionStatus::Running,
            log_file_path: None,
            started_at: 0,
            ended_at: None,
        };
        // An already-terminal row must be left untouched (no wasted write, no churn).
        let killed = Session {
            id: "killed".into(),
            status: SessionStatus::Killed,
            ..stale.clone()
        };
        store.insert_session(&stale).unwrap();
        store.insert_session(&killed).unwrap();

        let manager = SessionManager::new(AdapterRegistry::new(), TmuxController, store.clone());
        assert_eq!(manager.reconcile_with_tmux().unwrap(), 1);

        assert_eq!(store.get_session("stale").unwrap().unwrap().status, SessionStatus::Exited);
        assert_eq!(store.get_session("killed").unwrap().unwrap().status, SessionStatus::Killed);

        // Idempotent: a second pass has nothing left to reconcile.
        assert_eq!(manager.reconcile_with_tmux().unwrap(), 0);
    }

    #[test]
    fn find_session_reports_not_found_for_an_unknown_id() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let manager = SessionManager::new(AdapterRegistry::new(), TmuxController, store);
        let err = manager.find_session("nope").unwrap_err();
        assert!(matches!(err, SessionManagerError::SessionNotFound(ref id) if id == "nope"));
    }

    #[test]
    fn hire_creates_session_row_and_tmux_session() {
        if !tmux_available() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let mut registry = AdapterRegistry::new();
        registry.register(Box::new(FakeFactory));
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        store.conn_for_test_insert_project("p1");
        let manager = SessionManager::new(registry, TmuxController, store.clone());

        let profile = reins_core::HarnessProfile {
            id: "fake".into(), display_name: "Fake".into(),
            strengths: vec![], constraints: vec![], notes: String::new(),
        };
        let session = manager
            .hire("fake", profile, "p1", Path::new("/tmp"), Some("Architect".into()), None)
            .unwrap();

        assert_eq!(session.status, SessionStatus::Starting);
        let listed = store.list_sessions(Some("p1")).unwrap();
        assert_eq!(listed.len(), 1);

        manager.release(&session.tmux_session_name, &session.id).unwrap();
    }
}
