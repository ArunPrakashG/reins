use crate::{AdapterFactory, HarnessAdapter, SpawnContext, TerminalSnapshot};
use reins_core::{ConversationTurn, HarnessProfile, HarnessStatus, TurnRole};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ClaudeCodeAdapter {
    profile: HarnessProfile,
}

pub struct ClaudeCodeAdapterFactory;

impl AdapterFactory for ClaudeCodeAdapterFactory {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn create(&self, profile: HarnessProfile) -> Box<dyn HarnessAdapter> {
        Box::new(ClaudeCodeAdapter { profile })
    }
}

impl HarnessAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    fn spawn_command(&self, ctx: &SpawnContext) -> Command {
        let mut cmd = Command::new("claude");
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
        if screen.text.contains("esc to interrupt") {
            HarnessStatus::Running
        } else if screen.text.trim_end().ends_with('>') {
            HarnessStatus::AwaitingInput
        } else {
            HarnessStatus::Idle
        }
    }

    fn log_dir(&self, ctx: &SpawnContext) -> PathBuf {
        let encoded = ctx.project_path.to_string_lossy().replace('/', "-");
        crate::home_dir().join(".claude").join("projects").join(encoded)
    }

    fn parse_log(&self, path: &Path) -> Vec<ConversationTurn> {
        parse_claude_code_jsonl(path)
    }
}

fn parse_claude_code_jsonl(path: &Path) -> Vec<ConversationTurn> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .map(|v| {
            let role = match v.get("type").and_then(|t| t.as_str()) {
                Some("user") => TurnRole::User,
                Some("assistant") => TurnRole::Assistant,
                _ => TurnRole::Tool,
            };
            let content = v
                .pointer("/message/content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let tool_calls_json = v
                .pointer("/message/tool_calls")
                .map(|tc| tc.to_string());
            let timestamp = v.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0);
            ConversationTurn {
                role,
                content,
                tool_calls_json,
                timestamp,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_transcript_into_turns() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude_code_session.jsonl");
        let turns = parse_claude_code_jsonl(&path);

        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[0].content, "Add OAuth login");
        assert_eq!(turns[1].role, TurnRole::Assistant);
        assert!(turns[2].tool_calls_json.is_some());
    }

    #[test]
    fn detect_status_reads_running_banner() {
        let profile = HarnessProfile {
            id: "claude-code".into(),
            display_name: "Claude Code".into(),
            strengths: vec![],
            constraints: vec![],
            notes: String::new(),
        };
        let adapter = ClaudeCodeAdapter { profile };
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
