use crate::types::DerivedCancelCauseView;

#[derive(Debug, Clone, Default)]
pub struct RequestEvidence {
    pub interrupt_requested_at: Option<String>,
    pub caused_by_parent_request_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolCallEvidence {
    pub lifecycle_state: Option<String>,
    pub deadline_at: Option<String>,
    pub cancel_policy: Option<String>,
    pub completed_at: Option<String>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ResponseEvidence {
    pub interrupted_at: Option<String>,
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

    if req.caused_by_parent_request_id.is_some() && tool.cancel_policy.as_deref() == Some("cascade")
    {
        let parent = req.caused_by_parent_request_id.clone().unwrap_or_default();
        return Some(DerivedCancelCauseView {
            cause: "interrupted".into(),
            source: "parentCascade".into(),
            confidence: "derived".into(),
            at: tool.completed_at.clone(),
            evidence: vec![
                format!("AgentRequest.caused_by_parent_request_id = {parent}"),
                "AgentToolCall.cancel_policy = \"cascade\"".into(),
            ],
        });
    }

    if req.interrupt_requested_at.is_some() && req.caused_by_parent_request_id.is_none() {
        let at = req.interrupt_requested_at.clone();
        return Some(DerivedCancelCauseView {
            cause: "userCancelled".into(),
            source: "requestInterrupt".into(),
            confidence: "direct".into(),
            at: at.clone(),
            evidence: vec![
                format!(
                    "AgentRequest.interrupt_requested_at = {}",
                    at.as_deref().unwrap_or("(unset)"),
                ),
                "no parent cascade (caused_by_parent_request_id is null)".into(),
            ],
        });
    }

    Some(DerivedCancelCauseView {
        cause: "unknown".into(),
        source: "unresolved".into(),
        confidence: "derived".into(),
        at: tool.completed_at.clone(),
        evidence: vec![
            "checked: no parent cascade (caused_by_parent_request_id is null)".into(),
            "checked: no deadline (lifecycle_state is not timedOut)".into(),
            "checked: no interrupt_requested_at on root".into(),
            "schema has no persisted AgentToolCall.cancel_cause".into(),
        ],
    })
}

pub fn derive_response_cause(
    _req: &RequestEvidence,
    resp: &ResponseEvidence,
) -> Option<DerivedCancelCauseView> {
    if let Some(at) = &resp.interrupted_at {
        return Some(DerivedCancelCauseView {
            cause: "interrupted".into(),
            source: "responseInterruptedAt".into(),
            confidence: "direct".into(),
            at: Some(at.clone()),
            evidence: vec![format!("AgentResponse.interrupted_at = {at}")],
        });
    }
    None
}
