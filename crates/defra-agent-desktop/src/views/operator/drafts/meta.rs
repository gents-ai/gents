use crate::client::ClientStore;
use crate::state::OperatorSection;

use super::super::recent_failures::recent_failure_summaries;

pub(super) fn section_meta(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
) -> (&'static str, String) {
    match section {
        OperatorSection::Runtime => (
            "Runtime",
            selected_agent_did
                .and_then(|agent_did| store.latest_runtime(agent_did))
                .and_then(|runtime| runtime.process_state.clone())
                .unwrap_or_else(|| "current behavior, health, loop state".to_string()),
        ),
        OperatorSection::Behaviors => (
            "Behaviors",
            format!(
                "{} profiles",
                store
                    .behaviors
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                    .count()
            ),
        ),
        OperatorSection::Backends => (
            "Backends",
            format!("{} inference backends", store.inference_backends.len()),
        ),
        OperatorSection::ToolSelections => (
            "Tool selections",
            format!(
                "{} presets",
                store
                    .tool_selections
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                    .count()
            ),
        ),
        OperatorSection::InferenceProfiles => (
            "Inference profiles",
            format!("{} profiles", store.inference_profiles.len()),
        ),
        OperatorSection::ScheduledTasks => (
            "Scheduled Tasks",
            format!(
                "{} tasks",
                store
                    .scheduled_tasks
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                    .count()
            ),
        ),
        OperatorSection::RequestTimeline => (
            "Request Timeline",
            format!(
                "{} requests",
                store
                    .requests
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                    .count()
            ),
        ),
        OperatorSection::RecentFailures => (
            "Recent Failures",
            format!(
                "{} failures",
                recent_failure_summaries(store, selected_agent_did).len()
            ),
        ),
    }
}
