use crate::types::DerivedCancelCauseView;

#[derive(Debug, Clone, Default)]
pub struct RequestEvidence {
    pub interrupt_requested_at: Option<String>,
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
                format!("AgentToolCall.lifecycle_state = \"timedOut\""),
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

    // An interrupt latch reaches only its own request: no cancellation
    // crosses from one request to another, so the latch is this row's cause.
    if req.interrupt_requested_at.is_some() {
        let at = req.interrupt_requested_at.clone();
        return Some(DerivedCancelCauseView {
            cause: "userCancelled".into(),
            source: "requestInterrupt".into(),
            confidence: "direct".into(),
            at: at.clone(),
            evidence: vec![format!(
                "AgentRequest.interrupt_requested_at = {}",
                at.as_deref().unwrap_or("(unset)"),
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
            "checked: no interrupt_requested_at on root".into(),
            "no persisted AgentToolCall.cancel_cause on this row".into(),
        ],
    })
}

pub fn derive_request_cause(
    lifecycle_state: Option<&str>,
    req: &RequestEvidence,
    terminalized_at: Option<String>,
) -> Option<DerivedCancelCauseView> {
    if let Some(at) = &req.interrupt_requested_at {
        return Some(DerivedCancelCauseView {
            cause: "userCancelled".into(),
            source: "requestInterrupt".into(),
            confidence: "direct".into(),
            at: Some(at.clone()),
            evidence: vec![format!("AgentRequest.interrupt_requested_at = {at}")],
        });
    }
    if !matches!(lifecycle_state, Some("interrupted")) {
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
