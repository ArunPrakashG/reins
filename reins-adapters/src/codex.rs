use crate::{AdapterFactory, HarnessAdapter, SpawnContext, TerminalSnapshot};
use reins_core::{ConversationTurn, HarnessProfile, HarnessStatus, TurnRole};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct CodexAdapter {
    profile: HarnessProfile,
}

pub struct CodexAdapterFactory;

impl AdapterFactory for CodexAdapterFactory {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn create(&self, profile: HarnessProfile) -> Box<dyn HarnessAdapter> {
        Box::new(CodexAdapter { profile })
    }
}

impl HarnessAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    fn spawn_command(&self, ctx: &SpawnContext) -> Command {
        let mut cmd = Command::new("codex");
        cmd.current_dir(&ctx.project_path);
        if let Some(brief) = &ctx.brief {
            cmd.arg(brief);
        }
        cmd
    }

    fn interrupt_keys(&self) -> &[u8] {
        b"\x03" // Ctrl-C
    }

    fn detect_status(&self, screen: &TerminalSnapshot) -> HarnessStatus {
        // NOTE: placeholder heuristic, unconfirmed against real Codex CLI UI text.
        // Mirrors ClaudeCodeAdapter's approach until real banner text is verified.
        if screen.text.contains("esc to interrupt") {
            HarnessStatus::Running
        } else if screen.text.trim_end().ends_with('>') {
            HarnessStatus::AwaitingInput
        } else {
            HarnessStatus::Idle
        }
    }

    fn log_dir(&self, _ctx: &SpawnContext) -> PathBuf {
        // NOTE: real Codex CLI writes one JSONL file per session under
        // ~/.codex/sessions/YYYY/MM/DD/. This returns the sessions root;
        // recursively finding the newest .jsonl file under YYYY/MM/DD
        // subdirectories is a known simplification left for a later task.
        crate::home_dir().join(".codex").join("sessions")
    }

    fn parse_log(&self, path: &Path) -> Vec<ConversationTurn> {
        parse_codex_jsonl(path)
    }
}

fn parse_codex_jsonl(path: &Path) -> Vec<ConversationTurn> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let v = serde_json::from_str::<serde_json::Value>(line).ok()?;
            Some((line, v))
        })
        .map(|(line, v)| {
            let timestamp = v.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0);

            if v.get("type").and_then(|t| t.as_str()) == Some("response_item")
                && v.pointer("/payload/type").and_then(|t| t.as_str()) == Some("message")
            {
                let role = match v.pointer("/payload/role").and_then(|r| r.as_str()) {
                    Some("user") => TurnRole::User,
                    Some("assistant") => TurnRole::Assistant,
                    _ => TurnRole::Tool,
                };
                let content = v
                    .pointer("/payload/content")
                    .and_then(|c| c.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();

                ConversationTurn {
                    role,
                    content,
                    tool_calls_json: None,
                    timestamp,
                }
            } else {
                ConversationTurn {
                    role: TurnRole::Tool,
                    content: String::new(),
                    tool_calls_json: Some(line.to_string()),
                    timestamp,
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_transcript_into_turns() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_session.jsonl");
        let turns = parse_codex_jsonl(&path);

        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[0].content, "Add OAuth login");
        assert_eq!(turns[1].role, TurnRole::Assistant);
        assert_eq!(turns[1].content, "I'll start by reading the auth module.");
        assert_eq!(turns[2].role, TurnRole::Tool);
        assert!(turns[2].tool_calls_json.is_some());
    }

    #[test]
    fn detect_status_reads_running_banner() {
        let profile = HarnessProfile {
            id: "codex".into(),
            display_name: "Codex".into(),
            strengths: vec![],
            constraints: vec![],
            notes: String::new(),
        };
        let adapter = CodexAdapter { profile };
        let running = TerminalSnapshot {
            text: "Thinking... (esc to interrupt)".into(),
        };
        assert_eq!(adapter.detect_status(&running), HarnessStatus::Running);

        let waiting = TerminalSnapshot {
            text: "some output\n>".into(),
        };
        assert_eq!(adapter.detect_status(&waiting), HarnessStatus::AwaitingInput);
    }
}
