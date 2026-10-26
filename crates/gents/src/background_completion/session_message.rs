use super::*;
use gents_protocol::error_chain::ErrorChain;

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
                error = %ErrorChain(&error),
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
    /// Native history contains no assignment bound to the caused request.
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
/// comes from the Goal version bound to its signed physical assignment. A
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
    /// The `<reason>` vocabulary of the completion notification. Settlement
    /// persists this reason as the row's `completionPending:<reason>` suffix
    /// (Lean `Recovery.sessionMessageHandoffReason`) and the completion
    /// redrive re-renders it from that suffix, so the first notice and a
    /// crash-before-notice replay render byte-identically and the snapshot
    /// never changes on replay. A complete assignment is the reasonless
    /// success form (`assignment_complete` names the status, it is not
    /// emitted as a reason); failed turns keep their terminal reasons.
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
        crate::goal::GoalStatus::Stopped => "assignment_stopped",
    }
}

pub(crate) async fn turn_assignment_handoff(
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
                match crate::goal::load_request_assignment(node, caused).await {
                    Ok(crate::goal::RequestAssignmentObservation::Assigned(goal)) => goal
                        .parsed_status()
                        .map(AssignmentObservation::Assigned)
                        .unwrap_or(AssignmentObservation::Unknown),
                    Ok(crate::goal::RequestAssignmentObservation::None) => {
                        AssignmentObservation::None
                    }
                    Ok(crate::goal::RequestAssignmentObservation::Unknown) => {
                        AssignmentObservation::Unknown
                    }
                    Err(error) => {
                        tracing::warn!(
                            caused_request_id = %caused.request_id,
                            error = %ErrorChain(&error),
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
    if !lifecycle.is_running()
        || lifecycle.kind() != crate::tool_call_lifecycle::ToolRowKind::SessionMessage
    {
        return Ok(false);
    }
    let doc_id = lifecycle
        .doc_id()
        .ok_or(BackgroundCompletionError::SessionMessageRowLacksPhysicalIdentity)?
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
        .ok_or(BackgroundCompletionError::CausedRequestLacksPhysicalIdentity)?;
    let Some(terminal) =
        crate::background_tools::load_caused_request_terminal(node, caused_doc_id).await?
    else {
        return Ok(false);
    };
    // The handoff is computed before settlement and is grounded in the
    // caused request's durable state and the canonical Goal, never in the
    // tool turn: a completed turn with an active/blocked assignment stays a
    // non-complete handoff. Computing it first lets the reason persist as
    // part of the row's `completionPending:<reason>` settlement record, so
    // the completion redrive can re-render the same evidence.
    let handoff = turn_assignment_handoff(node, &caused, &terminal).await;
    let handoff_reason = handoff.reason();
    if !lifecycle
        .settle_session_message(&terminal, handoff_reason)
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
        terminal.notification_status(),
        terminal.output(),
        handoff_reason,
        crate::lifecycle::RequestHopCause::Return,
    )
    .await?;
    Ok(true)
}

#[cfg(test)]
mod turn_assignment_tests {
    use super::*;
    use crate::tool_call_lifecycle::admission_fixture::PublishedSessionMessage;
    use crate::tool_call_lifecycle::AwaitMode;

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
        for terminal in [
            "request_failed",
            "request_dead",
            "request_interrupted",
            "request_superseded",
        ] {
            assert!(!reasons.contains(terminal));
        }
    }

    /// A completed turn must not read as assignment evidence: only a
    /// complete assignment is the reasonless success form.
    #[tokio::test]
    async fn completed_turn_with_live_goal_hands_off_a_non_complete_reason() {
        let PublishedSessionMessage {
            admission,
            caused_request_id,
            caused_request_doc_id,
            ..
        } = crate::tool_call_lifecycle::admission_fixture::published_session_message(
            crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                name: "handoff-live-goal".into(),
                real_identity: true,
                await_mode: AwaitMode::Background,
                ..Default::default()
            },
        )
        .await
        .expect("publish accepted session message");
        let node = &admission.node;
        let did = &admission.node_did;
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        crate::test_support::install_test_agent(node, did, "general").await;
        // The caused session carries an active Goal: a finished turn with a
        // live assignment is not assignment success.
        crate::goal::set_goal(
            node,
            did,
            &caused_session_id(node, &caused_request_doc_id).await,
            Some("hold"),
            Some(crate::goal::GoalStatus::Active),
            None,
        )
        .await
        .expect("persist an active Goal in the caused session");
        bind_caused_assignment(node, did, &caused_request_doc_id).await;
        crate::tool_call_lifecycle::admission_fixture::complete_child(
            node,
            &caused_request_id,
            did,
            "child done",
        )
        .await;

        let settled = settle_running_session_message_rows(node, did)
            .await
            .unwrap();
        assert_eq!(settled, 1, "the caused terminal settles the row");
        // One settle call persists the suffix, publishes the notice and
        // advances the row: the converged form is `completed` with the
        // reason rendered. The durable pre-delivery suffix is pinned in the
        // crash-before-notice regression, where the side effect is skipped.
        let row = tool_call_row(node, &tool_doc_id).await;
        assert_eq!(row["lifecycle_state"], "completed");
        assert_eq!(row["status"], "completed");

        let notifications =
            session_message_notifications(node, did, admission.tool.session_id()).await;
        assert_eq!(notifications.len(), 1);
        assert!(
            notifications[0].contains("<reason>assignment_active</reason>"),
            "first notice names the live assignment: {}",
            notifications[0]
        );

        // Replay: the side effects already converged; the redrive republishes
        // nothing and the notice stays byte-identical.
        let redrive =
            crate::tool_call_lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
                node, did,
            )
            .await
            .unwrap();
        assert_eq!(
            redrive.side_effects_converged, 0,
            "side effects already converged; the replay republishes nothing"
        );
        admission.node.shutdown().await;
        std::fs::remove_dir_all(&admission.path).expect("remove fixture");
    }

    /// No-goal and complete-goal sessions are unmistakably distinguished from
    /// each other and from a live assignment.
    #[tokio::test]
    async fn completed_turn_without_a_goal_hands_off_assignment_absent() {
        let PublishedSessionMessage {
            admission,
            caused_request_id,
            ..
        } = crate::tool_call_lifecycle::admission_fixture::published_session_message(
            crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                name: "handoff-no-goal".into(),
                real_identity: true,
                await_mode: AwaitMode::Background,
                ..Default::default()
            },
        )
        .await
        .expect("publish accepted session message");
        let node = &admission.node;
        let did = &admission.node_did;
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        crate::test_support::install_test_agent(node, did, "general").await;
        crate::tool_call_lifecycle::admission_fixture::complete_child(
            node,
            &caused_request_id,
            did,
            "child done",
        )
        .await;

        let settled = settle_running_session_message_rows(node, did)
            .await
            .unwrap();
        assert_eq!(settled, 1);
        // One settle call persists the suffix, publishes the notice and
        // advances the row; the notice is where the absent-assignment
        // distinction lives (a no-goal session never reads as success).
        let row = tool_call_row(node, &tool_doc_id).await;
        assert_eq!(row["status"], "completed");
        let notifications =
            session_message_notifications(node, did, admission.tool.session_id()).await;
        assert_eq!(notifications.len(), 1);
        assert!(
            notifications[0].contains("<reason>assignment_absent</reason>"),
            "first notice names the absent assignment: {}",
            notifications[0]
        );
        admission.node.shutdown().await;
        std::fs::remove_dir_all(&admission.path).expect("remove fixture");
    }

    #[tokio::test]
    async fn completed_turn_with_complete_goal_is_the_reasonless_success_form() {
        let PublishedSessionMessage {
            admission,
            caused_request_id,
            caused_request_doc_id,
            ..
        } = crate::tool_call_lifecycle::admission_fixture::published_session_message(
            crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                name: "handoff-complete-goal".into(),
                real_identity: true,
                await_mode: AwaitMode::Background,
                ..Default::default()
            },
        )
        .await
        .expect("publish accepted session message");
        let node = &admission.node;
        let did = &admission.node_did;
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        crate::test_support::install_test_agent(node, did, "general").await;
        crate::goal::set_goal(
            node,
            did,
            &caused_session_id(node, &caused_request_doc_id).await,
            Some("done"),
            Some(crate::goal::GoalStatus::Complete),
            None,
        )
        .await
        .expect("persist a complete Goal in the caused session");
        bind_caused_assignment(node, did, &caused_request_doc_id).await;
        crate::tool_call_lifecycle::admission_fixture::complete_child(
            node,
            &caused_request_id,
            did,
            "child done",
        )
        .await;

        let settled = settle_running_session_message_rows(node, did)
            .await
            .unwrap();
        assert_eq!(settled, 1);
        let row = tool_call_row(node, &tool_doc_id).await;
        assert_eq!(row["status"], "completed");
        let notifications =
            session_message_notifications(node, did, admission.tool.session_id()).await;
        assert_eq!(notifications.len(), 1);
        assert!(
            !notifications[0].contains("<reason>"),
            "a complete assignment stays reasonless: {}",
            notifications[0]
        );
        admission.node.shutdown().await;
        std::fs::remove_dir_all(&admission.path).expect("remove fixture");
    }

    /// The persisted suffix, not live assignment state, is the replay
    /// authority: settlement persists `completionPending:<reason>` and a
    /// crash before the notice leaves it outstanding; the redrive re-renders
    /// the suffix byte-identically and advances the row to `completed`. The
    /// replayed reason is invariant across a Goal status change between
    /// settlement and redrive (the source snapshot never changes on replay).
    #[tokio::test]
    async fn crash_before_notice_redrive_renders_the_persisted_assignment_reason() {
        let PublishedSessionMessage {
            admission,
            caused_request_id,
            caused_request_doc_id,
            ..
        } = crate::tool_call_lifecycle::admission_fixture::published_session_message(
            crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                name: "handoff-redrive".into(),
                real_identity: true,
                await_mode: AwaitMode::Background,
                ..Default::default()
            },
        )
        .await
        .expect("publish accepted session message");
        let node = &admission.node;
        let did = &admission.node_did;
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let session_id = admission.tool.session_id().to_owned();
        crate::test_support::install_test_agent(node, did, "general").await;
        let goal_session_id = caused_session_id(node, &caused_request_doc_id).await;
        crate::goal::set_goal(
            node,
            did,
            &goal_session_id,
            Some("hold"),
            Some(crate::goal::GoalStatus::Active),
            None,
        )
        .await
        .expect("persist an active Goal in the caused session");
        bind_caused_assignment(node, did, &caused_request_doc_id).await;
        crate::tool_call_lifecycle::admission_fixture::complete_child(
            node,
            &caused_request_id,
            did,
            "child done",
        )
        .await;

        // Settle the lifecycle row only: a crash before the notice side
        // effect leaves `completionPending:assignment_active` outstanding.
        let mut lifecycle = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &tool_doc_id,
            did,
            &session_id,
            Some(did),
        )
        .await
        .unwrap()
        .expect("rehydrated session-message row");
        let caused = match crate::session_message::observe_caused_request(node, &lifecycle)
            .await
            .unwrap()
        {
            crate::session_message::CausedObservation::Bound(caused) => caused,
            crate::session_message::CausedObservation::Unbound(reason) => {
                panic!("expected a bound caused request, got Unbound({reason})")
            }
            crate::session_message::CausedObservation::NotVisible => {
                panic!("expected a bound caused request, got NotVisible")
            }
        };
        let caused_doc_id = caused.doc_id.as_deref().unwrap().to_owned();
        let Some(terminal) =
            crate::background_tools::load_caused_request_terminal(node, &caused_doc_id)
                .await
                .unwrap()
        else {
            panic!("the completed child is a terminal observation");
        };
        let handoff = turn_assignment_handoff(node, &caused, &terminal).await;
        let handoff_reason = handoff.reason();
        assert_eq!(handoff_reason, Some("assignment_active"));
        assert!(lifecycle
            .settle_session_message(&terminal, handoff_reason)
            .await
            .unwrap());
        // Pre-delivery persisted snapshot: the suffix is durable before any
        // notification exists (lifecycle state is terminal; only the notice
        // side effect is outstanding).
        let row = tool_call_row(node, &tool_doc_id).await;
        assert_eq!(row["status"], "completionPending:assignment_active");
        assert!(
            session_message_notifications(node, did, &session_id)
                .await
                .is_empty(),
            "no notice exists yet: this is the crash-before-notice state"
        );

        // The assignment changes under the replay (the Goal completes), but
        // the redrive must re-render the persisted suffix, never re-observe
        // live state.
        crate::goal::set_goal(
            node,
            did,
            &goal_session_id,
            Some("hold"),
            Some(crate::goal::GoalStatus::Complete),
            None,
        )
        .await
        .expect("the Goal completes after settlement");

        // The redrive re-renders from the row's persisted suffix.
        let report =
            crate::tool_call_lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
                node, did,
            )
            .await
            .unwrap();
        assert_eq!(report.side_effects_converged, 1);
        let notifications = session_message_notifications(node, did, &session_id).await;
        assert_eq!(notifications.len(), 1);
        assert!(
            notifications[0].contains("<reason>assignment_active</reason>"),
            "replay renders the persisted assignment reason, not live state: {}",
            notifications[0]
        );
        let row = tool_call_row(node, &tool_doc_id).await;
        assert_eq!(row["status"], "completed", "side effects converged");
        admission.node.shutdown().await;
        std::fs::remove_dir_all(&admission.path).expect("remove fixture");
    }

    /// Native background completions keep their reasonless success form: the
    /// persisted suffix path is reserved for session-message settlement
    /// (verified through the real redrive in
    /// `crash_before_notice_redrive_renders_the_persisted_assignment_reason`
    /// and by the projection regression in the recovery module).
    #[tokio::test]
    async fn session_message_settlement_persists_assignment_reason_not_native_status() {
        // Non-session-message completions never call settle_session_message;
        // the accessor at tool_call_lifecycle.rs keeps formatting any reason
        // it is given, so the native vocabulary is unchanged there.
        let terminal = crate::tool_call_lifecycle::CausedRequestTerminal::Completed {
            output: "child done".into(),
        };
        assert_eq!(terminal.completion_reason(), None);
        assert_eq!(terminal.notification_status(), "completed");
    }

    async fn tool_call_row(node: &Arc<EmbeddedNode>, tool_doc_id: &str) -> serde_json::Value {
        let escaped = escape_graphql_string(tool_doc_id);
        let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state }} }}"#
        ))
        .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        response.data.unwrap()["AgentToolCall"][0].clone()
    }

    async fn session_message_notifications(
        node: &Arc<EmbeddedNode>,
        did: &str,
        session_id: &str,
    ) -> Vec<String> {
        let escaped = escape_graphql_string(session_id);
        let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        ))
        .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        let mut notifications = Vec::new();
        for row in response.data.unwrap()["AgentMessage"].as_array().unwrap() {
            let (_, message) = crate::session::load_canonical_message_from_node(
                node,
                row["_docID"].as_str().unwrap(),
                did,
                Some(did),
            )
            .await
            .expect("reconstruct canonical session message");
            if let crate::llm::message::Message::User { content } = message {
                for item in content {
                    if let crate::llm::message::UserContent::Text(text) = item {
                        if text.text.contains("<tool-completion") {
                            notifications.push(text.text);
                        }
                    }
                }
            }
        }
        notifications
    }

    async fn bind_caused_assignment(node: &Arc<EmbeddedNode>, did: &str, request_doc: &str) {
        let session = caused_session_id(node, request_doc).await;
        let goal = crate::goal::load_canonical_goal(node, did, &session)
            .await
            .unwrap()
            .unwrap();
        crate::config_client::ConfigAccess::local(node.clone()).write("test.bind_caused_assignment",
            &format!("mutation {{update_Goal(docID: \"{}\", input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
                escape_graphql_string(&goal.doc_id), escape_graphql_string(request_doc))).await.unwrap();
    }

    async fn caused_session_id(node: &Arc<EmbeddedNode>, caused_request_doc_id: &str) -> String {
        let doc_id = escape_graphql_string(caused_request_doc_id);
        let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ session_id }} }}"#
        ))
        .await;
        assert!(
            !response.has_errors(),
            "caused request lookup: {:#?}",
            response.errors
        );
        response.data.unwrap()["AgentRequest"][0]["session_id"]
            .as_str()
            .expect("the caused request names its session")
            .to_owned()
    }
}
