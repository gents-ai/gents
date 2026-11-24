use gents_protocol::request_lifecycle::RequestLifecycleState;

use crate::types::DerivedCancelCauseView;

#[derive(Debug, Clone, Default)]
pub struct RequestEvidence {
    /// When the root request was interrupted: its terminal time once its
    /// lifecycle is `interrupted`.
    pub interrupted_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolCallEvidence {
    pub lifecycle_state: Option<String>,
    pub deadline_at: Option<String>,
    pub completed_at: Option<String>,
    pub timed_out: bool,
}

fn is_cancelled_terminal(state: &Option<String>) -> bool {
    matches!(
        state.as_deref(),
        Some("cancelled") | Some("interrupted") | Some("timedOut")
    )
}

pub fn derive_tool_call_cause(
    req: &RequestEvidence,
    tool: &ToolCallEvidence,
) -> Option<DerivedCancelCauseView> {
    if !is_cancelled_terminal(&tool.lifecycle_state) {
        return None;
    }

    if tool.timed_out || tool.lifecycle_state.as_deref() == Some("timedOut") {
        return Some(DerivedCancelCauseView {
            cause: "deadline".into(),
            source: "toolLifecycle".into(),
            confidence: "derived".into(),
            at: tool.completed_at.clone(),
            evidence: vec![
                "AgentToolCall.lifecycle_state = \"timedOut\"".to_string(),
                format!(
                    "deadline_at = {:?}",
                    tool.deadline_at.as_deref().unwrap_or("(unset)")
                ),
                format!(
                    "completed_at = {:?}",
                    tool.completed_at.as_deref().unwrap_or("(unset)")
                ),
            ],
        });
    }

    if let Some(at) = &req.interrupted_at {
        return Some(DerivedCancelCauseView {
            cause: "interrupted".into(),
            source: "requestInterrupt".into(),
            confidence: "direct".into(),
            at: Some(at.clone()),
            evidence: vec![format!(
                "root AgentRequest.lifecycle_state = \"interrupted\" at {at}"
            )],
        });
    }

    Some(DerivedCancelCauseView {
        cause: "unknown".into(),
        source: "unresolved".into(),
        confidence: "derived".into(),
        at: tool.completed_at.clone(),
        evidence: vec![
            "checked: no deadline (lifecycle_state is not timedOut)".into(),
            "checked: root request not interrupted".into(),
            "no persisted AgentToolCall.cancel_cause on this row".into(),
        ],
    })
}

pub fn derive_request_cause(
    lifecycle_state: Option<RequestLifecycleState>,
    terminalized_at: Option<String>,
) -> Option<DerivedCancelCauseView> {
    if lifecycle_state != Some(RequestLifecycleState::Interrupted) {
        return None;
    }
    Some(DerivedCancelCauseView {
        cause: "interrupted".into(),
        source: "requestLifecycle".into(),
        confidence: "direct".into(),
        at: terminalized_at,
        evidence: vec!["AgentRequest.lifecycle_state = \"interrupted\"".into()],
    })
}
