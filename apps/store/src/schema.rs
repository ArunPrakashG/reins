pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    harness_id TEXT NOT NULL,
    role TEXT,
    tmux_session_name TEXT NOT NULL,
    status TEXT NOT NULL,
    log_file_path TEXT,
    started_at INTEGER NOT NULL,
    ended_at INTEGER
);
"#;
