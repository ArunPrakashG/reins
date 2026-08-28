use crate::tmux::TmuxController;
use reins_adapters::AdapterRegistry;
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
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
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
