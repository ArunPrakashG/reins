use crate::{AdapterFactory, HarnessAdapter, SpawnContext, TerminalSnapshot};
use reins_core::{ConversationTurn, HarnessProfile, HarnessStatus, TurnRole};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GeminiCliAdapter {
    profile: HarnessProfile,
}

pub struct GeminiCliAdapterFactory;

impl AdapterFactory for GeminiCliAdapterFactory {
    fn id(&self) -> &'static str {
        "gemini-cli"
    }

    fn create(&self, profile: HarnessProfile) -> Box<dyn HarnessAdapter> {
        Box::new(GeminiCliAdapter { profile })
    }
}

impl HarnessAdapter for GeminiCliAdapter {
    fn id(&self) -> &'static str {
        "gemini-cli"
    }

    fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    fn program_name(&self) -> &'static str {
        "gemini"
    }

    fn spawn_command(&self, ctx: &SpawnContext) -> Command {
        let mut cmd = Command::new(self.program_name());
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
        // NOTE: placeholder heuristic, unconfirmed against real Gemini CLI UI text.
        // Mirrors ClaudeCodeAdapter/CodexAdapter's approach until real banner text is verified.
        if screen.text.contains("esc to interrupt") {
            HarnessStatus::Running
        } else if screen.text.trim_end().ends_with('>') {
            HarnessStatus::AwaitingInput
        } else {
            HarnessStatus::Idle
        }
    }

    fn log_dir(&self, _ctx: &SpawnContext) -> PathBuf {
        // NOTE: Gemini CLI stores chat/session data under
        // ~/.gemini/tmp/<project_hash>/chats/, where <project_hash> is derived
        // from the project's absolute root path. The exact hashing algorithm is
        // unconfirmed from available research, so this returns the parent
        // ".gemini/tmp" directory containing all project-hash subdirectories.
        // Locating the specific <project_hash>/chats/ subdirectory for a given
        // project requires either scanning all subdirectories for the
        // newest-modified "chats/" folder after spawn time, or reverse-engineering
        // the hash algorithm — both deferred to a later task, same category of
        // deferred work as CodexAdapter::log_dir's non-recursive-search gap.
        crate::home_dir().join(".gemini").join("tmp")
    }

    fn parse_log(&self, path: &Path) -> Vec<ConversationTurn> {
        parse_gemini_cli_log(path)
    }
}

fn parse_gemini_cli_log(path: &Path) -> Vec<ConversationTurn> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    // NOTE: assumed format is a single JSON array of chat entries (not JSONL).
    // A chat history is more naturally represented as one array of turns for a
    // resumable session, unlike Claude Code/Codex's append-only JSONL event logs.
    // This is unconfirmed against a real Gemini CLI chats file.
    let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&content) else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|v| {
            let role = match v.get("role").and_then(|r| r.as_str()) {
                Some("user") => TurnRole::User,
                Some("model") => TurnRole::Assistant,
                _ => TurnRole::Tool,
            };
            let parts = v.get("parts").and_then(|p| p.as_array());
            let content = parts
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            let tool_calls_json = parts
                .map(|items| {
                    items
                        .iter()
                        .filter(|item| item.get("text").is_none())
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .filter(|non_text| !non_text.is_empty())
                .map(|non_text| serde_json::Value::Array(non_text).to_string());

            ConversationTurn {
                role,
                content,
                tool_calls_json,
                timestamp: 0,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_transcript_into_turns() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gemini_cli_session.json");
        let turns = parse_gemini_cli_log(&path);

        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[0].content, "Add OAuth login");
        assert_eq!(turns[1].role, TurnRole::Assistant);
        assert_eq!(turns[1].content, "I'll start by reading the auth module.");
        assert_eq!(turns[2].role, TurnRole::Assistant);
        assert_eq!(turns[2].content, "Calling read_file on auth.py");
        assert!(turns[2].tool_calls_json.is_some());
    }

    #[test]
    fn detect_status_reads_running_banner() {
        let profile = HarnessProfile {
            id: "gemini-cli".into(),
            display_name: "Gemini CLI".into(),
            strengths: vec![],
            constraints: vec![],
            notes: String::new(),
        };
        let adapter = GeminiCliAdapter { profile };
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
