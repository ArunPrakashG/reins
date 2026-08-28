use crate::HarnessProfile;

pub struct TaskDescription(pub String);

pub struct RoutingSuggestion {
    pub profile: HarnessProfile,
}

pub trait CapabilityRouter: Send + Sync {
    fn suggest(&self, task: &TaskDescription, profiles: &[HarnessProfile]) -> Vec<RoutingSuggestion>;
}

pub struct ManualRouter;

impl CapabilityRouter for ManualRouter {
    fn suggest(&self, _task: &TaskDescription, profiles: &[HarnessProfile]) -> Vec<RoutingSuggestion> {
        profiles
            .iter()
            .cloned()
            .map(|profile| RoutingSuggestion { profile })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str) -> HarnessProfile {
        HarnessProfile {
            id: id.into(), display_name: id.into(),
            strengths: vec![], constraints: vec![], notes: String::new(),
        }
    }

    #[test]
    fn manual_router_returns_all_profiles_unranked() {
        let router = ManualRouter;
        let task = TaskDescription("anything".into());
        let profiles = vec![profile("claude-code"), profile("codex")];

        let suggestions = router.suggest(&task, &profiles);

        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].profile.id, "claude-code");
        assert_eq!(suggestions[1].profile.id, "codex");
    }
}
