use reins_core::{HarnessProfile, Session};
use serde::{Deserialize, Serialize};

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
    ListSessions { project_path: Option<String> },
    ListHarnesses,
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
