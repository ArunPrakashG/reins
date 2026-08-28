mod error;
mod schema;

pub use error::StoreError;

use reins_core::{Session, SessionStatus};
use rusqlite::Connection;
use std::sync::Mutex;

pub trait ConversationStore: Send + Sync {
    fn insert_session(&self, session: &Session) -> Result<(), StoreError>;
    fn update_status(&self, id: &str, status: SessionStatus) -> Result<(), StoreError>;
    fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<Session>, StoreError>;
}

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(schema::SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

fn status_to_str(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Starting => "starting",
        SessionStatus::Running => "running",
        SessionStatus::AwaitingInput => "awaiting_input",
        SessionStatus::Exited => "exited",
        SessionStatus::Killed => "killed",
    }
}

fn status_from_str(s: &str) -> SessionStatus {
    match s {
        "starting" => SessionStatus::Starting,
        "running" => SessionStatus::Running,
        "awaiting_input" => SessionStatus::AwaitingInput,
        "exited" => SessionStatus::Exited,
        _ => SessionStatus::Killed,
    }
}

impl ConversationStore for SqliteStore {
    fn insert_session(&self, session: &Session) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO sessions (id, project_id, harness_id, role, tmux_session_name, status, log_file_path, started_at, ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                session.id,
                session.project_id,
                session.harness_id,
                session.role,
                session.tmux_session_name,
                status_to_str(session.status),
                session.log_file_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                session.started_at,
                session.ended_at,
            ],
        )?;
        Ok(())
    }

    fn update_status(&self, id: &str, status: SessionStatus) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = conn.execute(
            "UPDATE sessions SET status = ?1 WHERE id = ?2",
            rusqlite::params![status_to_str(status), id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<Session>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = if project_id.is_some() {
            conn.prepare(
                "SELECT id, project_id, harness_id, role, tmux_session_name, status, log_file_path, started_at, ended_at
                 FROM sessions WHERE project_id = ?1",
            )?
        } else {
            conn.prepare(
                "SELECT id, project_id, harness_id, role, tmux_session_name, status, log_file_path, started_at, ended_at
                 FROM sessions",
            )?
        };
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<Session> {
            let log_path: Option<String> = row.get(6)?;
            Ok(Session {
                id: row.get(0)?,
                project_id: row.get(1)?,
                harness_id: row.get(2)?,
                role: row.get(3)?,
                tmux_session_name: row.get(4)?,
                status: status_from_str(&row.get::<_, String>(5)?),
                log_file_path: log_path.map(std::path::PathBuf::from),
                started_at: row.get(7)?,
                ended_at: row.get(8)?,
            })
        };
        let rows = if let Some(pid) = project_id {
            stmt.query_map([pid], map_row)?
        } else {
            stmt.query_map([], map_row)?
        };
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> Session {
        Session {
            id: "s1".into(),
            project_id: "p1".into(),
            harness_id: "claude-code".into(),
            role: Some("Architect".into()),
            tmux_session_name: "reins-s1".into(),
            status: SessionStatus::Running,
            log_file_path: None,
            started_at: 100,
            ended_at: None,
        }
    }

    #[test]
    fn insert_and_list_round_trips() {
        let store = SqliteStore::open_in_memory().unwrap();
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO projects (id, path, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["p1", "/tmp/p1", "p1", 0],
        )
        .unwrap();
        drop(conn);
        store.insert_session(&sample_session()).unwrap();

        let sessions = store.list_sessions(Some("p1")).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "s1");
        assert_eq!(sessions[0].role.as_deref(), Some("Architect"));
    }

    #[test]
    fn update_status_changes_stored_row() {
        let store = SqliteStore::open_in_memory().unwrap();
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO projects (id, path, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["p1", "/tmp/p1", "p1", 0],
        )
        .unwrap();
        drop(conn);
        store.insert_session(&sample_session()).unwrap();

        store.update_status("s1", SessionStatus::Exited).unwrap();

        let sessions = store.list_sessions(None).unwrap();
        assert_eq!(sessions[0].status, SessionStatus::Exited);
    }
}
