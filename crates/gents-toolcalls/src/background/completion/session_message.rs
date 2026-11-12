use super::*;
use gents_session::session::message::RequestGoalAssignment;

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
        gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
        gents_protocol::tool_names::AGENT_MESSAGE_TOOL_NAME,
    )
}

pub async fn settle_running_session_message_rows_with_access(
    node: &Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    goals: &dyn gents_session::session::message::SessionMessageGoals,
    local_did: &str,
) -> Result<usize> {
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ {} }}) {{ {SESSION_MESSAGE_ROW_FIELDS} }} }}"#,
        running_session_message_filter(local_did)
    );
    let response = access.execute(&query).await?;
    let rows: Vec<SessionMessageRow> =
        serde_json::from_value(response["data"]["AgentToolCall"].clone())?;
    let mut settled = 0;
    for row in rows {
        match settle_row(node, access, goals, &row).await {
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

pub async fn settle_rows_after_request_update(
    node: &Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    goals: &dyn gents_session::session::message::SessionMessageGoals,
    local_did: &str,
    request_doc_id: &str,
) -> Result<usize> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let query = Query::select(
        Selection::doc_id()
            .field(fields::REQUEST_ID)
            .field(fields::LIFECYCLE_STATE)
            .field(fields::CAUSED_BY_PARENT_TOOL_CALL_DOC_ID),
    )
    .filter(Filter::eq(fields::DOC_ID, request_doc_id))
    .limit(1)
    .render();
    let response = access.read(&query).await?;
    let mut requests: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    let Some(request) = requests.pop() else {
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
    let response = access.execute(&query).await?;
    let rows: Vec<SessionMessageRow> =
        serde_json::from_value(response["data"]["AgentToolCall"].clone())?;
    let mut settled = 0;
    for row in rows {
        settled += usize::from(settle_row(node, access, goals, &row).await?);
    }
    Ok(settled)
}

async fn settle_row(
    node: &Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    goals: &dyn gents_session::session::message::SessionMessageGoals,
    row: &SessionMessageRow,
) -> Result<bool> {
    let Some(mut lifecycle) = access
        .transact_readonly("background.load_session_message", |txn| {
            let node = node.clone();
            Box::pin(async move {
                let Some(mut lifecycle) = ToolCallLifecycle::load_by_doc_id_in_txn(
                    txn,
                    node,
                    &row.doc_id,
                    &row.node_did,
                    &row.session_id,
                    row.requester_did.as_deref(),
                )
                .await?
                else {
                    return Ok::<_, anyhow::Error>(None);
                };
                if txn.actor_did() != Some(lifecycle.actor().as_str()) {
                    return Ok(None);
                }
                lifecycle.bind_agent_signer(txn.managed_agent_signer())?;
                Ok(Some(lifecycle))
            })
        })
        .await?
    else {
        return Ok(false);
    };
    settle_session_message_row(node, goals, &mut lifecycle).await
}

/// Whether the caused session carries an assignment, grounded in the
/// canonical Goal document rather than the tool turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AssignmentObservation {
    /// Native history contains no assignment bound to the caused request.
    None,
    /// The Goal's durable status is the authoritative assignment state.
    Assigned(gents_protocol::goal_status::GoalStatus),
    /// The Goal is unreadable or carries an unknown status; the handoff must
    /// not claim assignment success.
    Unknown,
}

/// Turn-versus-assignment completion metadata for a session-message
/// child-result handoff. `turn_completed` comes from the caused request's
/// durable lifecycle state (via `CausedRequestTerminal`); the assignment
/// comes from the Goal version bound to its signed physical assignment. A
/// completed tool turn never implies assignment success: a live or blocked
/// assignment is named in the handoff so the continuation owner cannot read a
/// finished turn as a finished goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnAssignmentHandoff {
    turn_completed: bool,
    assignment: AssignmentObservation,
    terminal_reason: Option<&'static str>,
}

impl TurnAssignmentHandoff {
    pub(crate) fn new(
        terminal: &crate::call::lifecycle::CausedRequestTerminal,
        assignment: AssignmentObservation,
    ) -> Self {
        Self {
            turn_completed: matches!(
                terminal,
                crate::call::lifecycle::CausedRequestTerminal::Completed { .. }
            ),
            assignment,
            terminal_reason: terminal.completion_reason(),
        }
    }

    /// The `<reason>` vocabulary of the completion notification. Settlement
    /// persists this reason as the row's `completionPending:<reason>` suffix
    /// (Lean `Recovery.sessionMessageHandoffReason`) and the completion
    /// redrive re-renders it from that suffix, so the first notice and a
    /// crash-before-notice replay render byte-identically and the snapshot
    /// never changes on replay. A complete assignment is the reasonless
    /// success form (`assignment_complete` names the status, it is not
    /// emitted as a reason); failed turns keep their terminal reasons.
    pub fn reason(&self) -> Option<&'static str> {
        if !self.turn_completed {
            return self.terminal_reason;
        }
        match self.assignment {
            AssignmentObservation::None => Some("assignment_absent"),
            AssignmentObservation::Unknown => Some("assignment_unknown"),
            AssignmentObservation::Assigned(gents_protocol::goal_status::GoalStatus::Complete) => {
                None
            }
            AssignmentObservation::Assigned(status) => Some(assignment_status_reason(status)),
        }
    }
}

fn assignment_status_reason(status: gents_protocol::goal_status::GoalStatus) -> &'static str {
    match status {
        gents_protocol::goal_status::GoalStatus::Active => "assignment_active",
        gents_protocol::goal_status::GoalStatus::Paused => "assignment_paused",
        gents_protocol::goal_status::GoalStatus::Blocked => "assignment_blocked",
        gents_protocol::goal_status::GoalStatus::UsageLimited => "assignment_usage_limited",
        gents_protocol::goal_status::GoalStatus::BudgetLimited => "assignment_budget_limited",
        gents_protocol::goal_status::GoalStatus::Complete => "assignment_complete",
        gents_protocol::goal_status::GoalStatus::Stopped => "assignment_stopped",
    }
}

pub async fn turn_assignment_handoff(
    goals: &dyn gents_session::session::message::SessionMessageGoals,
    access: &gents_store::config_client::ConfigAccess,
    caused: &AgentRequestRow,
    terminal: &crate::call::lifecycle::CausedRequestTerminal,
) -> TurnAssignmentHandoff {
    let turn_completed = matches!(
        terminal,
        crate::call::lifecycle::CausedRequestTerminal::Completed { .. }
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
                match goals.request_assignment(access, caused).await {
                    Ok(RequestGoalAssignment::Assigned(status)) => {
                        AssignmentObservation::Assigned(status)
                    }
                    Ok(RequestGoalAssignment::None) => AssignmentObservation::None,
                    Ok(RequestGoalAssignment::Unknown) => AssignmentObservation::Unknown,
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
    TurnAssignmentHandoff::new(terminal, assignment)
}

pub(crate) async fn settle_session_message_row(
    node: &Arc<EmbeddedNode>,
    goals: &dyn gents_session::session::message::SessionMessageGoals,
    lifecycle: &mut ToolCallLifecycle,
) -> Result<bool> {
    if !lifecycle.is_running()
        || lifecycle.kind() != crate::call::lifecycle::ToolRowKind::SessionMessage
    {
        return Ok(false);
    }
    let doc_id = lifecycle
        .doc_id()
        .context("session-message row lacks physical identity")?
        .to_owned();
    let calling_request_id = lifecycle.calling_request_id().await?;
    let caused = match crate::background::caused::observe_caused_request(node, lifecycle).await? {
        crate::background::caused::CausedObservation::Bound(caused) => caused,
        crate::background::caused::CausedObservation::NotVisible => return Ok(false),
        crate::background::caused::CausedObservation::Unbound(reason) => {
            const REASON: &str = "caused_request_unbound";
            if !lifecycle
                .fail_owned_with_completion_reason(reason, FailureClass::External, REASON)
                .await?
            {
                return Ok(false);
            }
            append_background_tool_completion(
                node,
                &lifecycle.access()?,
                lifecycle.session_id(),
                &calling_request_id,
                &doc_id,
                lifecycle.tool_name(),
                "failed",
                reason,
                Some(REASON),
                gents_protocol::request_lifecycle::RequestHopCause::Continuation,
            )
            .await?;
            return Ok(true);
        }
    };
    let caused_doc_id = caused
        .doc_id
        .as_deref()
        .context("caused request lacks physical identity")?;
    let Some(terminal) = crate::background::status::load_session_message_terminal(
        &lifecycle.access()?,
        caused_doc_id,
    )
    .await?
    else {
        return Ok(false);
    };
    // The handoff is computed before settlement and is grounded in the
    // caused request's durable state and the canonical Goal, never in the
    // tool turn: a completed turn with an active/blocked assignment stays a
    // non-complete handoff. Computing it first lets the reason persist as
    // part of the row's `completionPending:<reason>` settlement record, so
    // the completion redrive can re-render the same evidence.
    let handoff = turn_assignment_handoff(goals, &lifecycle.access()?, &caused, &terminal).await;
    let handoff_reason = handoff.reason();
    if !lifecycle
        .settle_session_message(&terminal, handoff_reason)
        .await?
    {
        return Ok(false);
    }
    append_background_tool_completion(
        node,
        &lifecycle.access()?,
        lifecycle.session_id(),
        &calling_request_id,
        &doc_id,
        lifecycle.tool_name(),
        terminal.notification_status(),
        terminal.output(),
        handoff_reason,
        gents_protocol::request_lifecycle::RequestHopCause::Return,
    )
    .await?;
    Ok(true)
}

#[cfg(test)]
#[path = "session_message_tests.rs"]
mod tests;

#[cfg(test)]
mod conformance {
    use super::{AssignmentObservation, TurnAssignmentHandoff};
    use crate::call::lifecycle::CausedRequestTerminal as Terminal;

    fn caused(name: &str) -> Terminal {
        terminal(name).unwrap_or_else(|| panic!("{name} is not a caused terminal"))
    }

    fn observation(name: &str) -> AssignmentObservation {
        match name {
            "absent" => AssignmentObservation::None,
            "unknown" => AssignmentObservation::Unknown,
            status => AssignmentObservation::Assigned(
                serde_json::from_value(serde_json::Value::from(status))
                    .unwrap_or_else(|error| panic!("{status} is not a Goal status: {error}")),
            ),
        }
    }

    #[test]
    fn settlement_reason_matches_lean() {
        gents_lean_vocab::families::recovery_session_message_settlement_reason::replay(
            |case: &gents_lean_vocab::families::recovery_session_message_settlement_reason::Input<
                String,
                String,
            >| {
                TurnAssignmentHandoff::new(&caused(&case.terminal), observation(&case.assignment))
                    .reason()
            },
        );
    }

    #[test]
    fn settled_state_matches_lean() {
        gents_lean_vocab::families::recovery_caused_terminal_settled_state::replay(
            |case: &gents_lean_vocab::families::recovery_caused_terminal_settled_state::Input<
                String,
            >| caused(&case.terminal).tool_state(),
        );
    }

    fn terminal(name: &str) -> Option<Terminal> {
        match name {
            "pending" => None,
            "completed" => Some(Terminal::Completed {
                output: String::new(),
            }),
            "failed" => Some(Terminal::Failed {
                reason: String::new(),
            }),
            "superseded" => Some(Terminal::Superseded),
            "dead" => Some(Terminal::Dead),
            "interrupted" => Some(Terminal::Interrupted),
            _ => panic!("unknown modeled outcome {name}"),
        }
    }

    #[test]
    fn folded_outcome_matches_lean() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().agent_message_outcome_cases;
        assert_eq!(cases.len(), 5);
        for case in cases {
            let actual = crate::background::status::select_folded_outcome(
                case.folded,
                terminal(&case.caused),
                terminal(&case.head),
            );
            assert_eq!(actual, terminal(&case.expected), "{case:?}");
        }
    }
}
