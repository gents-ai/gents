use super::*;

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCommandPolicyCase {
    pub(crate) name: String,
    pub(crate) category: String,
    pub(crate) mode: String,
    pub(crate) allowed_argv_prefixes: Vec<Vec<String>>,
    pub(crate) forbidden_argv_prefixes: Vec<Vec<String>>,
    pub(crate) network_mode: String,
    pub(crate) read_only_allowlist: Vec<String>,
    pub(crate) command: String,
    pub(crate) lookup_command: String,
    pub(crate) args: Vec<String>,
    pub(crate) decision: String,
    pub(crate) denial_reason: Option<String>,
    pub(crate) matched_prefix: Option<Vec<String>>,
    pub(crate) denied_argv: Option<Vec<String>>,
    pub(crate) denied_command: Option<String>,
    pub(crate) denied_argument: Option<String>,
    pub(crate) denied_subcommand: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCommandSandboxCase {
    pub(crate) name: String,
    pub(crate) category: String,
    pub(crate) mode: String,
    pub(crate) workspace_write_sandbox_enforced: bool,
    pub(crate) decision: String,
    pub(crate) sandbox: Option<String>,
    pub(crate) denial_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCommandEnvCase {
    pub(crate) name: String,
    pub(crate) env_key: String,
    pub(crate) input_present: bool,
    pub(crate) input_name: String,
    pub(crate) input_value: String,
    pub(crate) output_name: String,
    pub(crate) expected_value_kind: Option<String>,
    pub(crate) expected_output_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanLiveOverlayCase {
    pub(crate) name: String,
    #[serde(rename = "liveOutputAvailable")]
    pub(crate) live_output_available: bool,
    #[serde(rename = "hasDurableOwner")]
    pub(crate) has_durable_owner: bool,
    #[serde(rename = "precedingToolCalls")]
    pub(crate) preceding_tool_calls: u64,
    #[serde(rename = "turnTerminal")]
    pub(crate) turn_terminal: bool,
    #[serde(rename = "turnLabel")]
    pub(crate) turn_label: String,
    #[serde(rename = "hasContent")]
    pub(crate) has_content: bool,
    #[serde(rename = "hasReasoning")]
    pub(crate) has_reasoning: bool,
    #[serde(rename = "expectOverlay")]
    pub(crate) expect_overlay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRequestProgressCase {
    pub(crate) name: String,
    #[serde(rename = "lifecycleState")]
    pub(crate) lifecycle_state: String,
    pub(crate) label: String,
    pub(crate) animated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPendingUserTurnCase {
    pub(crate) name: String,
    #[serde(rename = "hasDurableUserOwner")]
    pub(crate) has_durable_user_owner: bool,
    #[serde(rename = "unrelatedUserTurns")]
    pub(crate) unrelated_user_turns: u64,
    #[serde(rename = "expectPendingTurn")]
    pub(crate) expect_pending_turn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanQueuedSteeringTraceCase {
    pub(crate) name: String,
    pub(crate) actions: Vec<LeanQueuedSteeringAction>,
    #[serde(rename = "requestId")]
    pub(crate) request_id: u64,
    #[serde(rename = "requestDocId")]
    pub(crate) request_doc_id: u64,
    #[serde(rename = "contentToken")]
    pub(crate) content_token: u64,
    pub(crate) entry: LeanQueuedSteeringEntry,
    #[serde(rename = "interruptAt")]
    pub(crate) interrupt_at: Option<u64>,
    #[serde(rename = "preparedCandidate")]
    pub(crate) prepared_candidate: Option<LeanQueuedSteeringCandidate>,
    pub(crate) capture: LeanQueuedSteeringCapture,
    #[serde(rename = "lifecycleState")]
    pub(crate) lifecycle_state: String,
    #[serde(rename = "acceptedInput")]
    pub(crate) accepted_input: bool,
    #[serde(rename = "queueActive")]
    pub(crate) queue_active: Option<u64>,
    #[serde(rename = "admissionVisible")]
    pub(crate) admission_visible: bool,
    #[serde(rename = "canonicalAuthoredCount")]
    pub(crate) canonical_authored_count: u64,
    #[serde(rename = "providerSendPermitted")]
    pub(crate) provider_send_permitted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LeanQueuedSteeringAction {
    Enqueue,
    ClaimWithoutBegin,
    ClaimAndBegin,
    LatchInterrupt,
    InterruptBeforeClaim,
    AdmissionReject,
    FailBeforeStream,
    DedupLose,
    Expire,
    InterruptClaimed,
    InterruptProcessing,
    Fail,
    Finish,
    BindWorkspace,
    Claim,
    BeginInference,
    ContinueProcessing,
    Publish,
    PrepareFails,
    Capture,
    Send,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanQueuedSteeringEntry {
    #[serde(rename = "requestId")]
    pub(crate) request_id: u64,
    #[serde(rename = "createdAt")]
    pub(crate) created_at: u64,
    pub(crate) source: String,
    pub(crate) policy: String,
    #[serde(rename = "queueKey")]
    pub(crate) queue_key: Option<u64>,
    #[serde(rename = "queuedAfter")]
    pub(crate) queued_after: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanQueuedSteeringCandidate {
    pub(crate) closing: super::canonical_output::LeanCanonicalSegment,
    pub(crate) message:
        super::canonical_output::LeanCanonicalMessage<super::canonical_output::LeanPayloadSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanQueuedSteeringCapture {
    #[serde(rename = "agentDid")]
    pub(crate) agent_did: u64,
    #[serde(rename = "sessionId")]
    pub(crate) session_id: u64,
    #[serde(rename = "requestDocId")]
    pub(crate) request_doc_id: u64,
    #[serde(rename = "turnIndex")]
    pub(crate) turn_index: u64,
    pub(crate) attempt: u64,
    #[serde(rename = "bodyToken")]
    pub(crate) body_token: u64,
    #[serde(rename = "priorBodyToken")]
    pub(crate) prior_body_token: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanQueuedSteeringGuardCase {
    pub(crate) name: String,
    #[serde(rename = "prefixAdmitted")]
    pub(crate) prefix_admitted: bool,
    pub(crate) admitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityPrincipal {
    pub(crate) did: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityBehavior {
    pub(crate) id: String,
    pub(crate) principal: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityStructuralCase {
    pub(crate) name: String,
    pub(crate) principals: Vec<LeanIdentityPrincipal>,
    pub(crate) behaviors: Vec<LeanIdentityBehavior>,
    pub(crate) well_formed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityPermissionGrant {
    pub(crate) principal: String,
    pub(crate) permission: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityPermissionCase {
    pub(crate) name: String,
    pub(crate) principals: Vec<LeanIdentityPrincipal>,
    pub(crate) behaviors: Vec<LeanIdentityBehavior>,
    pub(crate) grants: Vec<LeanIdentityPermissionGrant>,
    pub(crate) permission: String,
    pub(crate) row_owner: String,
    pub(crate) actor_principal: String,
    pub(crate) actor_behavior: String,
    pub(crate) peer_principal: String,
    pub(crate) peer_behavior: String,
    pub(crate) expected_actor_principal: Option<String>,
    pub(crate) expected_peer_principal: Option<String>,
    pub(crate) expected_actor_allowed: bool,
    pub(crate) expected_peer_allowed: bool,
    pub(crate) same_principal: bool,
    pub(crate) expected_decisions_equal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityContract {
    pub(crate) name: String,
    pub(crate) statement: String,
    pub(crate) enforced: bool,
    pub(crate) tracked_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanQueueDeadlineConformanceCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) action: String,
    pub(crate) session_id: usize,
    pub(crate) legal: bool,
    pub(crate) pre_active_request_id: Option<usize>,
    pub(crate) post_active_request_id: Option<usize>,
    pub(crate) pre_pending_request_ids: Vec<usize>,
    pub(crate) post_pending_request_ids: Vec<usize>,
    pub(crate) claimed_request_id: Option<usize>,
    pub(crate) blocked_by_active: bool,
    pub(crate) superseded_request_ids: Vec<usize>,
    pub(crate) queue_key: Option<String>,
    pub(crate) post_coalesced_pending_count: usize,
    pub(crate) automated_drained_request_ids: Vec<usize>,
    pub(crate) preserved_user_pending_request_ids: Vec<usize>,
    pub(crate) preserved_foreign_requester_request_ids: Vec<usize>,
    pub(crate) preserved_foreign_owner_request_ids: Vec<usize>,
    pub(crate) post_terminal_request_ids: Vec<usize>,
    pub(crate) pre_request_deadline: Option<usize>,
    pub(crate) synthesized_claim_deadline: Option<usize>,
    pub(crate) post_deadline: Option<usize>,
    pub(crate) explicit_deadline_preserved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRecoverySweepCase {
    pub(crate) name: String,
    pub(crate) sweep_id: String,
    pub(crate) collection: String,
    pub(crate) rust_function: String,
    pub(crate) cadence: String,
    pub(crate) implementation_status: String,
    pub(crate) pre_state: String,
    pub(crate) terminal_state: String,
    pub(crate) measure_before: usize,
    pub(crate) measure_after: usize,
    pub(crate) deadline_expired: Option<bool>,
    pub(crate) unclaimed_expired: Option<bool>,
    pub(crate) parent_live: Option<bool>,
    pub(crate) parent_interrupted: Option<bool>,
    pub(crate) parent_terminal: Option<bool>,
    pub(crate) execution_registered: Option<bool>,
    pub(crate) process_outcome: Option<String>,
    pub(crate) owner_task_deleted: Option<bool>,
    pub(crate) recovery_cause: Option<String>,
    pub(crate) notification_reason: Option<String>,
    pub(crate) deadline_audit_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanReservedChildBinding {
    pub(crate) child: usize,
    pub(crate) agent: usize,
    pub(crate) behavior: usize,
    pub(crate) parent_request: usize,
    pub(crate) parent_request_doc: usize,
    pub(crate) parent_tool: usize,
    pub(crate) parent_tool_doc: usize,
    pub(crate) payload: usize,
    pub(crate) depth: usize,
    pub(crate) workspace: Option<super::canonical_execution::LeanCanonicalDelegatedWorkspace>,
    pub(crate) admission: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanReservedChildMaterializationCase {
    pub(crate) name: String,
    pub(crate) stored: Vec<LeanReservedChildBinding>,
    pub(crate) candidate: LeanReservedChildBinding,
    pub(crate) expected_decision: String,
    pub(crate) expected_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanLocalParentDepthExpected {
    Admitted { child_depth: u32 },
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanLocalParentDepthCase {
    pub(crate) name: String,
    pub(crate) supplied_parent_depth: u32,
    pub(crate) stored_parent_depth: Option<i64>,
    pub(crate) expected: LeanLocalParentDepthExpected,
}

/// Startup restart-disposition witness (#937): the shape of one running
/// `AgentToolCall` row and what `ToolCallLifecycle::recover_all` must do with
/// it — terminalize with a pinned cause/terminal state (plus, for the native
/// background interrupt, a durable notification and coalesced wake), or leave
/// the row running.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRestartDispositionCase {
    pub(crate) name: String,
    pub(crate) rust_function: String,
    pub(crate) await_mode: String,
    pub(crate) cancel_policy: String,
    pub(crate) child_linked: bool,
    pub(crate) parent_observation: String,
    pub(crate) deadline_expired: bool,
    pub(crate) unclaimed_expired: bool,
    pub(crate) process_outcome: String,
    pub(crate) child_observed: bool,
    pub(crate) bridge_cancel_intent: Option<bool>,
    pub(crate) bridge_ack_pending: Option<bool>,
    pub(crate) disposition: String,
    pub(crate) cause: Option<String>,
    pub(crate) terminal_state: Option<String>,
    pub(crate) post_await_mode: Option<String>,
    pub(crate) notification_reason: Option<String>,
    pub(crate) queue_source: Option<String>,
    pub(crate) queue_key_prefix: Option<String>,
    #[allow(dead_code)]
    pub(crate) theorem: String,
}
