use super::*;

#[derive(Debug, Deserialize)]
struct SessionMessageRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    session_id: String,
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
}

const SESSION_MESSAGE_ROW_FIELDS: &str = "_docID session_id node_did requester_did";

fn running_session_message_filter(local_did: &str) -> String {
    format!(
        r#"node_did: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "running" }}, await_mode: {{ _eq: "background" }}, spawned_by_tool_call_doc_id: {{ _eq: null }}, tool_name: {{ _in: ["{}", "{}"] }}"#,
        escape_graphql_string(local_did),
        crate::toolset::AGENT_NEW_TOOL_NAME,
        crate::toolset::AGENT_MESSAGE_TOOL_NAME,
    )
}

pub(crate) async fn settle_running_session_message_rows(
    node: &Arc<EmbeddedNode>,
    local_did: &str,
) -> Result<usize> {
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ {} }}) {{ {SESSION_MESSAGE_ROW_FIELDS} }} }}"#,
        running_session_message_filter(local_did)
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node.as_ref(),
        &query,
        "load running session-message rows",
    )
    .await?;
    let rows = crate::graphql::rows::<SessionMessageRow>(&response, "AgentToolCall")?;
    let mut settled = 0;
    for row in rows {
        match settle_row(node, &row).await {
            Ok(true) => settled += 1,
            Ok(false) => {}
            Err(error) => tracing::warn!(
                tool_call_doc_id = %row.doc_id,
                error = %format!("{error:#}"),
                "session-message row settlement failed; will retry"
            ),
        }
    }
    Ok(settled)
}

pub(super) async fn settle_rows_after_request_update(
    node: &Arc<EmbeddedNode>,
    local_did: &str,
    request_doc_id: &str,
) -> Result<usize> {
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
            _docID request_id lifecycle_state caused_by_parent_tool_call_doc_id
        }} }}"#,
        escape_graphql_string(request_doc_id)
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node.as_ref(),
        &query,
        "load updated request for session-message settlement",
    )
    .await?;
    let Some(request) = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
    else {
        return Ok(0);
    };
    if !request
        .lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
    {
        return Ok(0);
    }
    let Some(tool_doc_id) = request.caused_by_parent_tool_call_doc_id else {
        return Ok(0);
    };
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }}, {} }}, limit: 1) {{ {SESSION_MESSAGE_ROW_FIELDS} }} }}"#,
        escape_graphql_string(&tool_doc_id),
        running_session_message_filter(local_did)
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node.as_ref(),
        &query,
        "load the session-message row a terminal request names",
    )
    .await?;
    let mut settled = 0;
    for row in crate::graphql::rows::<SessionMessageRow>(&response, "AgentToolCall")? {
        settled += usize::from(settle_row(node, &row).await?);
    }
    Ok(settled)
}

async fn settle_row(node: &Arc<EmbeddedNode>, row: &SessionMessageRow) -> Result<bool> {
    let Some(mut lifecycle) = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &row.doc_id,
        &row.node_did,
        &row.session_id,
        row.requester_did.as_deref(),
    )
    .await?
    else {
        return Ok(false);
    };
    settle_session_message_row(node, &mut lifecycle).await
}

/// Whether the caused session carries an assignment, grounded in the
/// canonical Goal document rather than the tool turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AssignmentObservation {
    /// No Goal document exists in the caused session.
    None,
    /// The Goal's durable status is the authoritative assignment state.
    Assigned(crate::goal::GoalStatus),
    /// The Goal is unreadable or carries an unknown status; the handoff must
    /// not claim assignment success.
    Unknown,
}

/// Turn-versus-assignment completion metadata for a session-message
/// child-result handoff. `turn_completed` comes from the caused request's
/// durable lifecycle state (via `CausedRequestTerminal`); the assignment
/// comes from the canonical Goal of the session that request runs in. A
/// completed tool turn never implies assignment success: a live or blocked
/// assignment is named in the handoff so the continuation owner cannot read a
/// finished turn as a finished goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnAssignmentHandoff {
    turn_completed: bool,
    assignment: AssignmentObservation,
    terminal_reason: Option<&'static str>,
}

impl TurnAssignmentHandoff {
    /// The `<reason>` vocabulary of the completion notification. Reasons are
    /// durable-replay evidence: the redrive
    /// (`tool_call_lifecycle::recovery::background_completion_projection`)
    /// must reproduce them from the row's persisted
    /// `completionPending:<reason>` status, which requires the status lane to
    /// (a) persist this handoff reason in `settle_session_message` and
    /// (b) pass the persisted suffix through the projection's `completed`
    /// arm. Until that interface lands, a replay whose notice was never
    /// created renders the reasonless form; replays of an existing notice
    /// are already byte-stable (notification_delivery reuses stored text).
    /// A reasonless notification is only authoritative as assignment success
    /// once the handoff reason is durably reproducible.
    pub(crate) fn reason(&self) -> Option<&'static str> {
        if !self.turn_completed {
            return self.terminal_reason;
        }
        match self.assignment {
            AssignmentObservation::None => Some("assignment_absent"),
            AssignmentObservation::Unknown => Some("assignment_unknown"),
            AssignmentObservation::Assigned(crate::goal::GoalStatus::Complete) => None,
            AssignmentObservation::Assigned(status) => Some(assignment_status_reason(status)),
        }
    }
}

fn assignment_status_reason(status: crate::goal::GoalStatus) -> &'static str {
    match status {
        crate::goal::GoalStatus::Active => "assignment_active",
        crate::goal::GoalStatus::Paused => "assignment_paused",
        crate::goal::GoalStatus::Blocked => "assignment_blocked",
        crate::goal::GoalStatus::UsageLimited => "assignment_usage_limited",
        crate::goal::GoalStatus::BudgetLimited => "assignment_budget_limited",
        crate::goal::GoalStatus::Complete => "assignment_complete",
    }
}

async fn turn_assignment_handoff(
    node: &Arc<EmbeddedNode>,
    caused: &AgentRequestRow,
    terminal: &crate::tool_call_lifecycle::CausedRequestTerminal,
) -> TurnAssignmentHandoff {
    let terminal_reason = terminal.completion_reason();
    let turn_completed = matches!(
        terminal,
        crate::tool_call_lifecycle::CausedRequestTerminal::Completed { .. }
    );
    // Only a completed turn needs the assignment read; a failed turn already
    // carries its own terminal reason.
    let assignment = if !turn_completed {
        AssignmentObservation::None
    } else {
        match (caused.node_did.as_deref(), caused.session_id.as_deref()) {
            (Some(node_did), Some(session_id))
                if !node_did.trim().is_empty() && !session_id.trim().is_empty() =>
            {
                match crate::goal::load_canonical_goal(node, node_did, session_id).await {
                    Ok(Some(goal)) => goal
                        .parsed_status()
                        .map(AssignmentObservation::Assigned)
                        .unwrap_or(AssignmentObservation::Unknown),
                    Ok(None) => AssignmentObservation::None,
                    Err(error) => {
                        tracing::warn!(
                            caused_request_id = %caused.request_id,
                            error = %format!("{error:#}"),
                            "assignment state is unreadable; the handoff cannot claim assignment success"
                        );
                        AssignmentObservation::Unknown
                    }
                }
            }
            _ => {
                tracing::warn!(
                    caused_request_id = %caused.request_id,
                    "caused request lacks its session scope; the handoff cannot claim assignment success"
                );
                AssignmentObservation::Unknown
            }
        }
    };
    TurnAssignmentHandoff {
        turn_completed,
        assignment,
        terminal_reason,
    }
}

pub(crate) async fn settle_session_message_row(
    node: &Arc<EmbeddedNode>,
    lifecycle: &mut ToolCallLifecycle,
) -> Result<bool> {
    if !lifecycle.is_running() || !lifecycle.is_session_message() {
        return Ok(false);
    }
    let doc_id = lifecycle
        .doc_id()
        .context("session-message row lacks physical identity")?
        .to_owned();
    let calling_request_id =
        crate::session_message::calling_request_id(node.as_ref(), lifecycle).await?;
    let caused = match crate::session_message::observe_caused_request(node, lifecycle).await? {
        crate::session_message::CausedObservation::Bound(caused) => caused,
        crate::session_message::CausedObservation::NotVisible => return Ok(false),
        crate::session_message::CausedObservation::Unbound(reason) => {
            const REASON: &str = "caused_request_unbound";
            if !lifecycle
                .fail_owned_with_completion_reason(reason, FailureClass::External, REASON)
                .await?
            {
                return Ok(false);
            }
            append_background_tool_completion(
                node,
                lifecycle.session_id(),
                &calling_request_id,
                &doc_id,
                lifecycle.tool_name(),
                "failed",
                reason,
                Some(REASON),
                crate::lifecycle::RequestHopCause::Continuation,
            )
            .await?;
            return Ok(true);
        }
    };
    let caused_doc_id = caused
        .doc_id
        .as_deref()
        .context("caused request lacks physical identity")?;
    let Some(terminal) =
        crate::background_tools::load_caused_request_terminal(node, caused_doc_id).await?
    else {
        return Ok(false);
    };
    if !lifecycle.settle_session_message(&terminal).await? {
        return Ok(false);
    }
    // The handoff is grounded in the caused request's durable state and the
    // canonical Goal, never in the tool turn: a completed turn with an
    // active/blocked assignment stays a non-complete handoff.
    let handoff = turn_assignment_handoff(node, &caused, &terminal).await;
    append_background_tool_completion(
        node,
        lifecycle.session_id(),
        &calling_request_id,
        &doc_id,
        lifecycle.tool_name(),
        terminal.notification_status(),
        terminal.output(),
        handoff.reason(),
        crate::lifecycle::RequestHopCause::Return,
    )
    .await?;
    Ok(true)
}

#[cfg(test)]
mod turn_assignment_tests {
    use super::*;

    fn handoff(
        turn_completed: bool,
        assignment: AssignmentObservation,
        terminal_reason: Option<&'static str>,
    ) -> TurnAssignmentHandoff {
        TurnAssignmentHandoff {
            turn_completed,
            assignment,
            terminal_reason,
        }
    }

    #[test]
    fn a_completed_turn_with_a_live_assignment_does_not_report_assignment_success() {
        for status in [
            crate::goal::GoalStatus::Active,
            crate::goal::GoalStatus::Blocked,
            crate::goal::GoalStatus::Paused,
            crate::goal::GoalStatus::UsageLimited,
            crate::goal::GoalStatus::BudgetLimited,
        ] {
            let handoff = handoff(true, AssignmentObservation::Assigned(status), None);
            assert_eq!(
                handoff.reason(),
                Some(assignment_status_reason(status)),
                "{status:?}: a finished turn must name its non-complete assignment"
            );
            assert_ne!(handoff.reason(), Some("assignment_complete"));
        }
    }

    #[test]
    fn a_completed_turn_with_a_complete_assignment_is_a_clean_success() {
        let handoff = handoff(
            true,
            AssignmentObservation::Assigned(crate::goal::GoalStatus::Complete),
            None,
        );
        assert_eq!(handoff.reason(), None);
    }

    #[test]
    fn a_completed_turn_without_a_goal_is_distinguished_from_assignment_success() {
        let handoff = handoff(true, AssignmentObservation::None, None);
        assert_eq!(handoff.reason(), Some("assignment_absent"));
    }

    #[test]
    fn an_unreadable_assignment_never_reads_as_success() {
        let handoff = handoff(true, AssignmentObservation::Unknown, None);
        assert_eq!(handoff.reason(), Some("assignment_unknown"));
    }

    #[test]
    fn a_failed_turn_keeps_its_terminal_reason_not_an_assignment_reason() {
        for (terminal, reason) in [
            (
                crate::tool_call_lifecycle::CausedRequestTerminal::Failed {
                    reason: "backend failed".into(),
                },
                Some("request_failed"),
            ),
            (
                crate::tool_call_lifecycle::CausedRequestTerminal::Dead,
                Some("request_dead"),
            ),
            (
                crate::tool_call_lifecycle::CausedRequestTerminal::Interrupted,
                Some("request_interrupted"),
            ),
            (
                crate::tool_call_lifecycle::CausedRequestTerminal::Superseded,
                Some("request_superseded"),
            ),
        ] {
            // The assignment read is skipped for failed turns even when the
            // session carries a live Goal: the terminal reason decides.
            let handoff = handoff(
                false,
                AssignmentObservation::Assigned(crate::goal::GoalStatus::Active),
                reason,
            );
            assert_eq!(handoff.reason(), reason, "{terminal:?}");
            assert_eq!(reason, terminal.completion_reason());
        }
    }

    #[test]
    fn the_live_reason_vocabulary_is_distinct_from_the_terminal_vocabulary() {
        let reasons: std::collections::BTreeSet<&str> = [
            "assignment_absent",
            "assignment_active",
            "assignment_paused",
            "assignment_blocked",
            "assignment_usage_limited",
            "assignment_budget_limited",
            "assignment_unknown",
        ]
        .into_iter()
        .collect();
        assert_eq!(reasons.len(), 7);
        for terminal in ["request_failed", "request_dead", "request_interrupted", "request_superseded"] {
            assert!(!reasons.contains(terminal));
        }
    }
}
