use reins_core::{HarnessProfile, Session};
use serde::{Deserialize, Serialize};

/// A single keystroke to forward into a hired harness's tmux pane, in whichever shape
/// tmux's own `send-keys` wants it. Two shapes rather than one raw-byte blob because
/// tmux distinguishes them itself: a `Named` token is looked up against tmux's own
/// key-name vocabulary ("Enter", "Left", "C-c", ...), while `Literal` text is sent
/// verbatim (`send-keys -l`) so ordinary typed characters can never be misread as a
/// key name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum KeyInput {
    Literal { text: String },
    Named { token: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    Hire {
        harness_id: String,
        project_path: String,
        role: Option<String>,
        brief: Option<String>,
    },
    Release { session_id: String },
    Interrupt { session_id: String },
    /// Forwards one keystroke into a session's tmux pane — the general-purpose sibling
    /// of `Interrupt`, which only ever sends the harness's fixed interrupt sequence.
    SendKeys { session_id: String, input: KeyInput },
    ListSessions { project_path: Option<String> },
    ListHarnesses,
    /// Requests the most recently captured tmux pane content for a session, with color
    /// and cursor information for live rendering. On-demand passthrough for MVP: the
    /// daemon captures the pane fresh on each request rather than maintaining a
    /// background poller. A future streaming upgrade (a persistent per-session byte
    /// stream) would replace this.
    GetPaneSnapshot { session_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum Response {
    Ok { result: ResponseBody },
    Err { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseBody {
    Session(Session),
    Sessions(Vec<Session>),
    Harnesses(Vec<HarnessProfile>),
    /// Pane content captured for a `GetPaneSnapshot` request: `text` carries tmux's
    /// `capture-pane -e` output (color/style escape codes included, for the TUI's
    /// `vt100`-backed renderer), `cursor` is the pane's `(x, y)` cursor position from a
    /// separate `display-message` query — `capture-pane` itself doesn't encode where
    /// the cursor actually is.
    PaneSnapshot { text: String, cursor: (u16, u16) },
    Empty,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hire_request_round_trips_through_json() {
        let req = Request::Hire {
            harness_id: "claude-code".into(),
            project_path: "/tmp/proj".into(),
            role: Some("Architect".into()),
            brief: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        match back {
            Request::Hire { harness_id, .. } => assert_eq!(harness_id, "claude-code"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn err_response_round_trips_through_json() {
        let resp = Response::Err { message: "boom".into() };
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        match back {
            Response::Err { message } => assert_eq!(message, "boom"),
            _ => panic!("wrong variant"),
        }
    }
}
