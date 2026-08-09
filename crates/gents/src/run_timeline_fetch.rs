//! Row fetch for [`crate::run_timeline`]: loads the persisted documents a
//! request's timeline is reconstructed from, over either transport
//! ([`ConfigAccess::Graphql`] or [`ConfigAccess::Local`]). Lifted from the
//! CLI `trace` command so the desktop client shares one fetcher.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config_client::ConfigAccess;
use crate::document_version::{
    document_field_version_ref_with_identity,
    verified_current_signed_document_version_with_executor,
    verified_current_signed_document_version_with_identity,
    verified_exact_document_snapshot_with_executor, verified_exact_document_snapshot_with_identity,
    VerifiedExactDocumentSnapshot,
};
use crate::graphql::escape_graphql_string;
use crate::run_timeline::{
    build_run_timeline, RunTimeline, RunTimelineRows, TimelineCompactionEntryRow,
    TimelineConversationRow, TimelineInferenceCallRow, TimelineMessageRow,
    TimelineRenderedRequestRow, TimelineRequestRow, TimelineResponseOutcomeRow,
    TimelineResponseRow, TimelineSessionRow, TimelineToolApprovalFact, TimelineToolCallRow,
    TimelineToolOutputOmissionFact, TimelineToolResultFact,
};
use crate::run_timeline_manifest::{
    freeze_timeline_manifest_with_declared_edges, RunTimelineSourceManifest, TimelineCoverageGap,
    TimelineCoverageGapKind, TimelineDeclaredExactEdge, TimelineExpectedSlot,
    TimelineObservedSource, TimelineRootCandidate, TimelineRootSelector, TimelineSlotRequirement,
    TimelineSourceClass, TimelineSourceDecision, TimelineSourceSlot,
};
use gents_protocol::graphql::graphql_rows_from_response;

const TIMELINE_REQUEST_SELECTION: &str = r#"
    request_id
    agent_did
    behavior_id
    session_id
    content
    metadata
    status
    lifecycle_state
    backend_id
    failure_reason
    created_at
    retry_count
    interrupt_requested_at
    caused_by_parent_request_id
    caused_by_parent_tool_call_id
"#;

const TIMELINE_RENDERED_REQUEST_SELECTION: &str = r#"
    capture_key
    request_doc_id
    request_source_commit_cid
    request_source_signer_did
    request_claim_commit_cid
    request_claim_signer_did
    inference_call_doc_id
    inference_call_composite_commit_cid
    inference_call_signer_did
    request_id
    session_id
    agent_did
    requester_did
    behavior_id
    capture_scope
    turn_index
    attempt
    capture_version
    model_name
    source
    prompt_hash
    tools_hash
    provenance_json
    created_at
"#;

const TIMELINE_RESPONSE_OUTCOME_SELECTION: &str = r#"
    request_doc_id
    request_id
    session_id
    agent_did
    requester_did
    behavior_id
    request_source_composite_commit_cid
    request_source_signer_did
    request_claim_composite_commit_cid
    request_claim_signer_did
    outcome_kind
    reason_code
    final_message_doc_id
    final_message_composite_commit_cid
    final_message_collection_version_id
    final_message_signer_did
    final_message_sequence
    terminalized_at
"#;

const TIMELINE_COMPACTION_SELECTION: &str = r#"
    compaction_key
    session_id
    agent_did
    requester_did
    sequence
    summary
    files_read
    files_modified
    messages_compacted
    original_tokens
    compacted_tokens
    source_manifest_version
    source_manifest_json
    created_at
    fork_source_doc_id
    fork_source_composite_commit_cid
    fork_source_signer_did
"#;

const TIMELINE_MESSAGE_SELECTION: &str = r#"
    session_id
    request_id
    request_doc_id
    agent_did
    sequence
    role
    content
    timestamp
"#;

const TIMELINE_TOOL_CALL_SELECTION: &str = r#"
    request_id
    session_id
    message_sequence
    tool_name
    tool_call_id
    args
    result
    result_doc_id
    result_composite_commit_cid
    result_signer_did
    omission_doc_id
    omission_composite_commit_cid
    omission_signer_did
    approval_doc_id
    approval_composite_commit_cid
    approval_signer_did
    status
    lifecycle_state
    started_at
    deadline_at
    completed_at
    selected_service_id
    selected_tool_name
    tool_failure_class
    denial_reason
    denied_argv
    denied_command
    denied_argument
    denied_subcommand
    denied_prefix
    policy_mode
    policy_network
    latency_ms
    await_mode
    cancel_policy
    cancel_cause
    child_request_id
"#;

const TIMELINE_RESPONSE_SELECTION: &str = r#"
    request_id
    agent_did
    behavior_id
    session_id
    content
    reasoning
    status
    error_message
    token_count
    progress_seq
    materialized_message_sequence
    materialized_at
    created_at
    completed_at
    interrupted_at
"#;

const TIMELINE_INFERENCE_CALL_SELECTION: &str = r#"
    call_id
    request_id
    call_seq
    attempt
    call_state
    failure_reason
    queued_at
    started_at
    ended_at
    backend_id
    call_kind
"#;

const TIMELINE_INFERENCE_ADMISSION_SELECTION: &str = r#"
    request_id
    behavior_id
    agent_did
    call_state
"#;

const TIMELINE_SESSION_SELECTION: &str = r#"
    session_id
    agent_name
    behavior_id
    started
    ended
    status
"#;

const TIMELINE_CONVERSATION_SELECTION: &str = r#"
    session_id
    agent_name
    agent_did
    behavior_id
    title
    title_source
    preview_text
    status
    created_at
    updated_at
    latest_request_id
    forked_from_session_id
"#;

pub async fn load_run_timeline(access: &ConfigAccess, request_id: &str) -> Result<RunTimeline> {
    Ok(build_run_timeline(
        load_run_timeline_rows(access, request_id).await?,
    ))
}

/// Reconstruct a timeline from one immutable DefraDB request snapshot.
///
/// The document id and composite commit CID are both required. A bare CID or
/// logical request id is not an exact document selector.
pub async fn load_run_timeline_exact(
    access: &ConfigAccess,
    request_doc_id: &str,
    request_composite_cid: &str,
) -> Result<RunTimeline> {
    Ok(build_run_timeline(
        load_run_timeline_rows_exact(access, request_doc_id, request_composite_cid).await?,
    ))
}

pub async fn load_run_timeline_rows(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<RunTimelineRows> {
    let (request, root_source) = load_timeline_request_by_id(access, request_id).await?;
    load_run_timeline_rows_from_root(access, request, root_source).await
}

/// Load the persisted rows contributing to a timeline rooted at one immutable
/// AgentRequest version.
pub async fn load_run_timeline_rows_exact(
    access: &ConfigAccess,
    request_doc_id: &str,
    request_composite_cid: &str,
) -> Result<RunTimelineRows> {
    let version = crate::DocumentVersionRef::new(request_doc_id, request_composite_cid);
    let (request, root_source) = load_timeline_request_exact(access, &version).await?;
    load_run_timeline_rows_from_root(access, request, root_source).await
}

async fn load_run_timeline_rows_from_root(
    access: &ConfigAccess,
    request: TimelineRequestRow,
    root_source: Option<ExactDocumentSource>,
) -> Result<RunTimelineRows> {
    let root_session_id = request.session_id.clone();

    let mut requests = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_requests_for_session(access, session_id).await?,
        None => Vec::new(),
    };
    ensure_unique_timeline_request_ids(&requests)?;
    merge_timeline_request(&mut requests, request.clone())?;
    for child in load_timeline_child_requests(access, &request.request_id).await? {
        merge_timeline_request(&mut requests, child)?;
    }

    let session_ids = timeline_session_ids(&requests);
    let mut messages = Vec::new();
    let mut tool_calls = Vec::new();
    let mut responses = Vec::new();
    for session_id in &session_ids {
        messages.extend(load_timeline_messages_for_session(access, session_id).await?);
        let mut session_tool_calls =
            load_timeline_tool_calls_for_session(access, session_id).await?;
        attach_exact_tool_facts(access, session_id, &mut session_tool_calls).await?;
        tool_calls.extend(session_tool_calls);
        responses.extend(load_timeline_responses_for_session(access, session_id).await?);
    }
    if session_ids.is_empty() || root_session_id.is_none() {
        responses.extend(load_timeline_responses_for_request(access, &request.request_id).await?);
    }
    let mut inference_calls = Vec::new();
    let request_ids = timeline_request_ids(&requests);
    for request_id in &request_ids {
        inference_calls
            .extend(load_timeline_inference_calls_for_request(access, request_id).await?);
    }
    reject_timeline_semantic_twins(&messages, &tool_calls, &responses, &inference_calls)?;
    let mut rendered_requests = Vec::new();
    for request_id in &request_ids {
        rendered_requests.extend(load_timeline_rendered_requests(access, request_id).await?);
    }
    reject_rendered_request_twins(&rendered_requests)?;
    rendered_requests.sort_by(|left, right| left.row.ordering_key().cmp(&right.row.ordering_key()));

    let mut response_outcomes = Vec::new();
    for request_doc_id in requests
        .iter()
        .filter_map(|request| request.doc_id.as_deref())
    {
        let mut request_outcomes = load_timeline_response_outcomes(access, request_doc_id).await?;
        if request_outcomes.len() > 1 {
            anyhow::bail!(
                "AgentResponseOutcome request _docID={request_doc_id} has {} visible siblings; refusing ambiguous terminal provenance",
                request_outcomes.len()
            );
        }
        response_outcomes.append(&mut request_outcomes);
    }

    let mut compaction_entries = Vec::new();
    for session_id in &session_ids {
        compaction_entries.extend(load_timeline_compaction_entries(access, session_id).await?);
    }
    reject_compaction_twins(&compaction_entries)?;
    compaction_entries.sort_by(|left, right| {
        left.row
            .sequence
            .cmp(&right.row.sequence)
            .then_with(|| left.row.compaction_key.cmp(&right.row.compaction_key))
            .then_with(|| left.row.doc_id.cmp(&right.row.doc_id))
    });

    let session = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_session(access, session_id).await?,
        None => None,
    };
    let conversation = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_conversation(access, session_id).await?,
        None => None,
    };

    let mut rows = RunTimelineRows {
        source_manifest: None,
        request,
        session,
        conversation,
        requests,
        messages,
        tool_calls,
        inference_calls,
        rendered_requests,
        response_outcomes,
        compaction_entries,
        responses,
    };
    if let (ConfigAccess::Local(node), Some(root_source)) = (access, root_source) {
        rows.source_manifest =
            Some(freeze_exact_timeline_rows(node, &mut rows, root_source).await?);
    }
    Ok(rows)
}

async fn load_timeline_request_by_id(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<(TimelineRequestRow, Option<ExactDocumentSource>)> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{}" }} }}
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    let rows = load_rows::<TimelineRequestRow>(access, "AgentRequest", &query).await?;
    let discovered = match rows.as_slice() {
        [] => anyhow::bail!("request {request_id} not found"),
        [request] => request,
        rows => anyhow::bail!(
            "request id {request_id} matches {} AgentRequest documents; use an exact document id and composite CID",
            rows.len()
        ),
    };
    let doc_id = discovered
        .doc_id
        .as_deref()
        .filter(|doc_id| !doc_id.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("request {request_id} omitted _docID"))?;
    match access {
        ConfigAccess::Local(node) => {
            let identity = timeline_reader_identity(node)?;
            let source = verified_current_signed_document_version_with_identity(
                node,
                "AgentRequest",
                doc_id,
                Some(identity),
            )
            .await?;
            let (request, exact) = load_timeline_request_exact(access, &source.version).await?;
            if request.request_id != request_id {
                anyhow::bail!(
                    "AgentRequest {doc_id} changed logical request id from {request_id} to {} while selecting its exact source",
                    request.request_id
                );
            }
            Ok((request, exact))
        }
        ConfigAccess::Graphql(graphql) => {
            let executor = crate::HttpGraphqlExecutor::new(graphql.clone());
            let source = verified_current_signed_document_version_with_executor(
                &executor,
                "AgentRequest",
                doc_id,
            )
            .await?;
            let (request, exact) = load_timeline_request_exact(access, &source.version).await?;
            if request.request_id != request_id {
                anyhow::bail!(
                    "AgentRequest {doc_id} changed logical request id from {request_id} to {} while selecting its exact source",
                    request.request_id
                );
            }
            Ok((request, exact))
        }
    }
}

async fn load_timeline_request_exact(
    access: &ConfigAccess,
    version: &crate::DocumentVersionRef,
) -> Result<(TimelineRequestRow, Option<ExactDocumentSource>)> {
    match access {
        ConfigAccess::Local(node) => {
            let identity = timeline_reader_identity(node)?;
            let snapshot = verified_exact_document_snapshot_with_identity(
                node,
                "AgentRequest",
                version,
                TIMELINE_REQUEST_SELECTION,
                Some(identity),
            )
            .await?;
            let request = snapshot.decode::<TimelineRequestRow>()?;
            Ok((request, Some(ExactDocumentSource::from(snapshot))))
        }
        ConfigAccess::Graphql(graphql) => {
            let executor = crate::HttpGraphqlExecutor::new(graphql.clone());
            let snapshot = verified_exact_document_snapshot_with_executor(
                &executor,
                "AgentRequest",
                version,
                TIMELINE_REQUEST_SELECTION,
            )
            .await?;
            let request = snapshot.decode::<TimelineRequestRow>()?;
            Ok((request, Some(ExactDocumentSource::from(snapshot))))
        }
    }
}

async fn load_timeline_requests_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineRequestRow>> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentRequest", &query).await
}

async fn load_timeline_child_requests(
    access: &ConfigAccess,
    parent_request_id: &str,
) -> Result<Vec<TimelineRequestRow>> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_parent_request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#,
        escape_graphql_string(parent_request_id)
    );
    load_rows(access, "AgentRequest", &query).await
}

async fn load_timeline_messages_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineMessageRow>> {
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                _docID
                session_id
                request_id
                request_doc_id
                agent_did
                sequence
                role
                content
                timestamp
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentMessage", &query).await
}

async fn load_timeline_tool_calls_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineToolCallRow>> {
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                _docID
                request_id
                session_id
                message_sequence
                tool_name
                tool_call_id
                args
                result
                result_doc_id
                result_composite_commit_cid
                result_signer_did
                omission_doc_id
                omission_composite_commit_cid
                omission_signer_did
                approval_doc_id
                approval_composite_commit_cid
                approval_signer_did
                status
                lifecycle_state
                started_at
                deadline_at
                completed_at
                selected_service_id
                selected_tool_name
                tool_failure_class
                denial_reason
                denied_argv
                denied_command
                denied_argument
                denied_subcommand
                denied_prefix
                policy_mode
                policy_network
                latency_ms
                await_mode
                cancel_policy
                cancel_cause
                child_request_id
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentToolCall", &query).await
}

#[derive(serde::Deserialize)]
struct TimelineResultFactRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    result_key: String,
    tool_call_key: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    session_id: String,
    output_text: String,
}

#[derive(serde::Deserialize)]
struct TimelineOmissionFactRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    omission_key: String,
    tool_call_key: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    session_id: String,
    source_phase: String,
    terminal_phase: String,
    reason: String,
    detail: String,
    created_at: Option<String>,
}

#[derive(serde::Deserialize)]
struct TimelineApprovalFactRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    approver_did: String,
    decision: String,
    reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct HistoricalToolCallFactRow {
    tool_call_id: String,
    session_id: String,
    lifecycle_state: String,
}

async fn exact_current_ref(
    access: &ConfigAccess,
    collection: &str,
    doc_id: &str,
) -> Result<crate::SignedDocumentVersionRef> {
    match access {
        ConfigAccess::Local(node) => {
            verified_current_signed_document_version_with_executor(
                node.as_ref(),
                collection,
                doc_id,
            )
            .await
        }
        ConfigAccess::Graphql(graphql) => {
            let executor = crate::HttpGraphqlExecutor::new(graphql.clone());
            verified_current_signed_document_version_with_executor(&executor, collection, doc_id)
                .await
        }
    }
}

fn complete_edge_doc_id<'a>(
    doc_id: Option<&'a str>,
    composite_commit_cid: Option<&str>,
    signer_did: Option<&str>,
    label: &str,
) -> Result<Option<&'a str>> {
    match (doc_id, composite_commit_cid, signer_did) {
        (None, None, None) => Ok(None),
        (Some(doc_id), Some(cid), Some(signer))
            if !doc_id.trim().is_empty() && !cid.trim().is_empty() && !signer.trim().is_empty() =>
        {
            Ok(Some(doc_id))
        }
        _ => anyhow::bail!("{label} exact reference is partial or empty"),
    }
}

async fn verify_historical_tool_call_ref(
    access: &ConfigAccess,
    source: &crate::SignedDocumentVersionRef,
) -> Result<HistoricalToolCallFactRow> {
    let snapshot = match access {
        ConfigAccess::Local(node) => {
            verified_exact_document_snapshot_with_executor(
                node.as_ref(),
                "AgentToolCall",
                &source.version,
                "tool_call_id session_id lifecycle_state",
            )
            .await?
        }
        ConfigAccess::Graphql(graphql) => {
            let executor = crate::HttpGraphqlExecutor::new(graphql.clone());
            verified_exact_document_snapshot_with_executor(
                &executor,
                "AgentToolCall",
                &source.version,
                "tool_call_id session_id lifecycle_state",
            )
            .await?
        }
    };
    if snapshot.source.signer_did != source.signer_did {
        anyhow::bail!("historical AgentToolCall signer does not match the pinned fact edge");
    }
    snapshot.decode()
}

fn terminal_tool_phase(call: &TimelineToolCallRow) -> Option<&str> {
    call.lifecycle_state
        .as_deref()
        .filter(|phase| matches!(*phase, "completed" | "failed" | "timedOut" | "cancelled"))
}

fn omission_reason_allows(reason: &str, source: &str, terminal: &str) -> bool {
    match reason {
        "preDispatchFailure" => source == "pending" && terminal == "failed",
        "approvalDenied" => source == "awaitingApproval" && terminal == "failed",
        "timedOut" => matches!(source, "running" | "awaitingApproval") && terminal == "timedOut",
        "cancelled" => {
            matches!(source, "pending" | "awaitingApproval" | "running") && terminal == "cancelled"
        }
        "recoveryFailure" | "executionLost" | "childDead" | "childSuperseded" => {
            source == "running" && terminal == "failed"
        }
        _ => false,
    }
}

fn validate_terminal_outcome_edge_shape(call: &TimelineToolCallRow) -> Result<()> {
    let result = complete_edge_doc_id(
        call.result_doc_id.as_deref(),
        call.result_composite_commit_cid.as_deref(),
        call.result_signer_did.as_deref(),
        "AgentToolCall result",
    )?;
    let omission = complete_edge_doc_id(
        call.omission_doc_id.as_deref(),
        call.omission_composite_commit_cid.as_deref(),
        call.omission_signer_did.as_deref(),
        "AgentToolCall omission",
    )?;
    if result.is_some() && omission.is_some() {
        anyhow::bail!(
            "terminal AgentToolCall {} binds both result and omission facts",
            call.tool_call_id
        );
    }
    if terminal_tool_phase(call).is_some() && result.is_none() && omission.is_none() {
        anyhow::bail!(
            "terminal AgentToolCall {} has no exact result or omission fact",
            call.tool_call_id
        );
    }
    Ok(())
}

fn validate_bound_outcome_signature(
    edge_cid: Option<&str>,
    edge_signer: Option<&str>,
    exact: &crate::SignedDocumentVersionRef,
    parent_signer: &str,
    label: &str,
) -> Result<()> {
    if edge_cid != Some(exact.version.composite_commit_cid.as_str())
        || edge_signer != Some(exact.signer_did.as_str())
        || exact.signer_did != parent_signer
    {
        anyhow::bail!("AgentToolCall {label} edge does not match exact signed {label} fact");
    }
    Ok(())
}

async fn attach_exact_tool_facts(
    access: &ConfigAccess,
    session_id: &str,
    calls: &mut [TimelineToolCallRow],
) -> Result<()> {
    for call in calls.iter() {
        validate_terminal_outcome_edge_shape(call)?;
    }
    let result_query = format!(
        r#"{{ AgentToolResult(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID result_key tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did session_id output_text }} }}"#,
        escape_graphql_string(session_id)
    );
    let results: Vec<TimelineResultFactRow> =
        load_rows(access, "AgentToolResult", &result_query).await?;
    let omission_query = format!(
        r#"{{ AgentToolOutputOmission(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID omission_key tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did session_id source_phase terminal_phase reason detail created_at }} }}"#,
        escape_graphql_string(session_id)
    );
    let omissions: Vec<TimelineOmissionFactRow> =
        load_rows(access, "AgentToolOutputOmission", &omission_query).await?;
    let approval_query = format!(
        r#"{{ AgentToolApproval(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did approver_did decision reason }} }}"#,
        escape_graphql_string(session_id)
    );
    let approvals: Vec<TimelineApprovalFactRow> =
        load_rows(access, "AgentToolApproval", &approval_query).await?;

    for call in calls {
        let call_doc_id = call.doc_id.as_deref().unwrap_or_default();
        if let Some(result_doc_id) = complete_edge_doc_id(
            call.result_doc_id.as_deref(),
            call.result_composite_commit_cid.as_deref(),
            call.result_signer_did.as_deref(),
            "AgentToolCall result",
        )? {
            let matching = results
                .iter()
                .filter(|row| row.doc_id == result_doc_id)
                .collect::<Vec<_>>();
            let [row] = matching.as_slice() else {
                anyhow::bail!(
                    "exact result ref resolved to {} physical rows",
                    matching.len()
                );
            };
            if row.tool_call_doc_id != call_doc_id {
                anyhow::bail!("result fact points to a different physical AgentToolCall");
            }
            if row.result_key != row.tool_call_composite_commit_cid
                || row.session_id != call.session_id
                || row.tool_call_key != format!("{}:{}", call.session_id, call.tool_call_id)
            {
                anyhow::bail!("AgentToolResult immutable binding does not match AgentToolCall");
            }
            let exact = exact_current_ref(access, "AgentToolResult", result_doc_id).await?;
            validate_bound_outcome_signature(
                call.result_composite_commit_cid.as_deref(),
                call.result_signer_did.as_deref(),
                &exact,
                &row.tool_call_signer_did,
                "result",
            )?;
            let historical = verify_historical_tool_call_ref(
                access,
                &crate::SignedDocumentVersionRef::new(
                    crate::DocumentVersionRef::new(
                        &row.tool_call_doc_id,
                        &row.tool_call_composite_commit_cid,
                    ),
                    &row.tool_call_signer_did,
                ),
            )
            .await?;
            if historical.tool_call_id != call.tool_call_id
                || historical.session_id != call.session_id
                || historical.lifecycle_state != "running"
            {
                anyhow::bail!("AgentToolResult does not bind the running historical AgentToolCall");
            }
            call.result_fact = Some(TimelineToolResultFact {
                doc_id: exact.version.doc_id,
                composite_commit_cid: exact.version.composite_commit_cid,
                signer_did: exact.signer_did,
                tool_call_doc_id: row.tool_call_doc_id.clone(),
                tool_call_composite_commit_cid: row.tool_call_composite_commit_cid.clone(),
                tool_call_signer_did: row.tool_call_signer_did.clone(),
                output_text: row.output_text.clone(),
            });
        }
        if let Some(omission_doc_id) = complete_edge_doc_id(
            call.omission_doc_id.as_deref(),
            call.omission_composite_commit_cid.as_deref(),
            call.omission_signer_did.as_deref(),
            "AgentToolCall omission",
        )? {
            let matching = omissions
                .iter()
                .filter(|row| row.doc_id == omission_doc_id)
                .collect::<Vec<_>>();
            let [row] = matching.as_slice() else {
                anyhow::bail!(
                    "exact omission ref resolved to {} physical rows",
                    matching.len()
                );
            };
            if row.tool_call_doc_id != call_doc_id
                || row.omission_key != row.tool_call_composite_commit_cid
                || row.session_id != call.session_id
                || row.tool_call_key != format!("{}:{}", call.session_id, call.tool_call_id)
            {
                anyhow::bail!(
                    "AgentToolOutputOmission immutable binding does not match AgentToolCall"
                );
            }
            let exact =
                exact_current_ref(access, "AgentToolOutputOmission", omission_doc_id).await?;
            validate_bound_outcome_signature(
                call.omission_composite_commit_cid.as_deref(),
                call.omission_signer_did.as_deref(),
                &exact,
                &row.tool_call_signer_did,
                "omission",
            )?;
            let historical = verify_historical_tool_call_ref(
                access,
                &crate::SignedDocumentVersionRef::new(
                    crate::DocumentVersionRef::new(
                        &row.tool_call_doc_id,
                        &row.tool_call_composite_commit_cid,
                    ),
                    &row.tool_call_signer_did,
                ),
            )
            .await?;
            if historical.tool_call_id != call.tool_call_id
                || historical.session_id != call.session_id
                || historical.lifecycle_state != row.source_phase
                || terminal_tool_phase(call) != Some(row.terminal_phase.as_str())
            {
                anyhow::bail!(
                    "AgentToolOutputOmission phase or historical call binding does not match AgentToolCall"
                );
            }
            if !omission_reason_allows(&row.reason, &row.source_phase, &row.terminal_phase) {
                anyhow::bail!(
                    "AgentToolOutputOmission reason {} does not permit {} -> {}",
                    row.reason,
                    row.source_phase,
                    row.terminal_phase
                );
            }
            call.omission_fact = Some(TimelineToolOutputOmissionFact {
                doc_id: exact.version.doc_id,
                composite_commit_cid: exact.version.composite_commit_cid,
                signer_did: exact.signer_did,
                tool_call_doc_id: row.tool_call_doc_id.clone(),
                tool_call_composite_commit_cid: row.tool_call_composite_commit_cid.clone(),
                tool_call_signer_did: row.tool_call_signer_did.clone(),
                source_phase: row.source_phase.clone(),
                terminal_phase: row.terminal_phase.clone(),
                reason: row.reason.clone(),
                detail: row.detail.clone(),
                created_at: row.created_at.clone(),
            });
        }
        if let Some(approval_doc_id) = complete_edge_doc_id(
            call.approval_doc_id.as_deref(),
            call.approval_composite_commit_cid.as_deref(),
            call.approval_signer_did.as_deref(),
            "AgentToolCall approval",
        )? {
            let matching = approvals
                .iter()
                .filter(|row| row.doc_id == approval_doc_id)
                .collect::<Vec<_>>();
            let [row] = matching.as_slice() else {
                anyhow::bail!(
                    "exact approval ref resolved to {} physical rows",
                    matching.len()
                );
            };
            if row.tool_call_doc_id != call.doc_id.as_deref().unwrap_or_default() {
                anyhow::bail!("approval fact points to a different physical AgentToolCall");
            }
            let exact = exact_current_ref(access, "AgentToolApproval", approval_doc_id).await?;
            if call.approval_composite_commit_cid.as_deref()
                != Some(exact.version.composite_commit_cid.as_str())
                || call.approval_signer_did.as_deref() != Some(exact.signer_did.as_str())
                || row.approver_did != exact.signer_did
            {
                anyhow::bail!(
                    "AgentToolCall approval edge does not match exact signed approval fact"
                );
            }
            let _ = verify_historical_tool_call_ref(
                access,
                &crate::SignedDocumentVersionRef::new(
                    crate::DocumentVersionRef::new(
                        &row.tool_call_doc_id,
                        &row.tool_call_composite_commit_cid,
                    ),
                    &row.tool_call_signer_did,
                ),
            )
            .await?;
            call.approval_fact = Some(TimelineToolApprovalFact {
                doc_id: exact.version.doc_id,
                composite_commit_cid: exact.version.composite_commit_cid,
                signer_did: exact.signer_did,
                tool_call_doc_id: row.tool_call_doc_id.clone(),
                tool_call_composite_commit_cid: row.tool_call_composite_commit_cid.clone(),
                tool_call_signer_did: row.tool_call_signer_did.clone(),
                decision: row.decision.clone(),
                reason: row.reason.clone(),
            });
        }
    }
    Ok(())
}

async fn load_timeline_responses_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineResponseRow>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                reasoning
                status
                error_message
                token_count
                progress_seq
                materialized_message_sequence
                materialized_at
                created_at
                completed_at
                interrupted_at
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentResponse", &query).await
}

async fn load_timeline_responses_for_request(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Vec<TimelineResponseRow>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                reasoning
                status
                error_message
                token_count
                progress_seq
                materialized_message_sequence
                materialized_at
                created_at
                completed_at
                interrupted_at
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    load_rows(access, "AgentResponse", &query).await
}

async fn load_timeline_inference_calls_for_request(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Vec<TimelineInferenceCallRow>> {
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ call_seq: ASC }}
            ) {{
                _docID
                call_id
                request_id
                call_seq
                attempt
                call_state
                failure_reason
                queued_at
                started_at
                ended_at
                backend_id
                call_kind
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    load_rows(access, "InferenceCall", &query).await
}

async fn load_timeline_rendered_requests(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Vec<TimelineRenderedRequestRow>> {
    // `request_json` is intentionally absent. Ordinary timeline and projection
    // reads expose only provenance metadata; sensitive body retrieval is a
    // separate exact-field operation.
    let query = format!(
        r#"{{
            RenderedRequest(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                capture_key
                request_doc_id
                request_source_commit_cid
                request_source_signer_did
                request_claim_commit_cid
                request_claim_signer_did
                inference_call_doc_id
                inference_call_composite_commit_cid
                inference_call_signer_did
                request_id
                session_id
                agent_did
                requester_did
                behavior_id
                capture_scope
                turn_index
                attempt
                capture_version
                model_name
                source
                prompt_hash
                tools_hash
                provenance_json
                created_at
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    Ok(
        load_rows::<gents_protocol::row::RenderedRequestRow>(access, "RenderedRequest", &query)
            .await?
            .into_iter()
            .map(|row| TimelineRenderedRequestRow {
                row,
                exact: None,
                request_json_field_cid: None,
            })
            .collect(),
    )
}

async fn load_timeline_response_outcomes(
    access: &ConfigAccess,
    request_doc_id: &str,
) -> Result<Vec<TimelineResponseOutcomeRow>> {
    let query = format!(
        r#"{{
            AgentResponseOutcome(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }}
            ) {{
                _docID
                request_doc_id
                request_id
                session_id
                agent_did
                requester_did
                behavior_id
                request_source_composite_commit_cid
                request_source_signer_did
                request_claim_composite_commit_cid
                request_claim_signer_did
                outcome_kind
                reason_code
                final_message_doc_id
                final_message_composite_commit_cid
                final_message_signer_did
                final_message_sequence
                terminalized_at
            }}
        }}"#,
        escape_graphql_string(request_doc_id)
    );
    Ok(load_rows::<gents_protocol::row::AgentResponseOutcomeRow>(
        access,
        "AgentResponseOutcome",
        &query,
    )
    .await?
    .into_iter()
    .map(|row| TimelineResponseOutcomeRow { row, exact: None })
    .collect())
}

async fn load_timeline_compaction_entries(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineCompactionEntryRow>> {
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                _docID
                compaction_key
                session_id
                agent_did
                requester_did
                sequence
                summary
                files_read
                files_modified
                messages_compacted
                original_tokens
                compacted_tokens
                source_manifest_version
                source_manifest_json
                created_at
                fork_source_doc_id
                fork_source_composite_commit_cid
                fork_source_signer_did
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    Ok(
        load_rows::<gents_protocol::row::CompactionEntryRow>(access, "CompactionEntry", &query)
            .await?
            .into_iter()
            .map(|row| TimelineCompactionEntryRow { row, exact: None })
            .collect(),
    )
}

async fn verified_current_exact_row<T>(
    node: &defra_node::EmbeddedNode,
    collection: &str,
    doc_id: &str,
    selection: &str,
    identity: &identity::Did,
) -> Result<(T, ExactDocumentSource)>
where
    T: DeserializeOwned,
{
    if doc_id.trim().is_empty() {
        anyhow::bail!("{collection} timeline source omitted _docID");
    }
    let current = verified_current_signed_document_version_with_identity(
        node,
        collection,
        doc_id,
        Some(identity.clone()),
    )
    .await?;
    let snapshot = verified_exact_document_snapshot_with_identity(
        node,
        collection,
        &current.version,
        selection,
        Some(identity.clone()),
    )
    .await?;
    if snapshot.source != current {
        anyhow::bail!(
            "{collection} {doc_id} signer evidence changed between current-head selection and exact reload"
        );
    }
    let row = snapshot.decode::<T>()?;
    Ok((row, ExactDocumentSource::from(snapshot)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExactDocumentSource {
    exact: crate::SignedDocumentVersionRef,
    /// DefraDB's composite commit named by `exact` cryptographically commits
    /// to this schema id in `_C`. Exact reload verifies that same CID and
    /// signer before extracting the id; this is not a current-schema lookup.
    collection_version_id: String,
}

impl From<VerifiedExactDocumentSnapshot> for ExactDocumentSource {
    fn from(snapshot: VerifiedExactDocumentSnapshot) -> Self {
        Self {
            exact: snapshot.source,
            collection_version_id: snapshot.collection_version_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExactTimelineSource {
    class: TimelineSourceClass,
    collection: &'static str,
    source: ExactDocumentSource,
}

impl ExactTimelineSource {
    fn declared_edge(&self) -> TimelineDeclaredExactEdge {
        TimelineDeclaredExactEdge {
            collection: self.collection.to_string(),
            collection_version_id: self.source.collection_version_id.clone(),
            exact: self.source.exact.clone(),
        }
    }
}

fn timeline_exact_source_selection(collection: &str) -> Result<&'static str> {
    match collection {
        "AgentRequest" => Ok("request_id"),
        "AgentSession" | "AgentConversation" | "AgentMessage" => Ok("session_id"),
        "AgentToolCall" => Ok("tool_call_id"),
        "AgentToolResult" => Ok("result_key"),
        "AgentToolOutputOmission" => Ok("omission_key"),
        "AgentToolApproval" => Ok("approval_id"),
        "AgentResponse" => Ok("request_id"),
        "AgentResponseOutcome" => Ok("request_doc_id"),
        "InferenceCall" => Ok("call_id"),
        "RenderedRequest" => Ok("capture_key"),
        "CompactionEntry" => Ok("compaction_key"),
        "AgentPrincipal" => Ok("agent_did"),
        "AgentBehavior" => Ok("behavior_id"),
        "InferenceBackend" => Ok("backend_id"),
        "InferenceProfile" => Ok("profile_id"),
        "ToolSelection" => Ok("selection_id"),
        "DatastoreToolSurface" => Ok("surface_id"),
        "Skill" => Ok("skill_id"),
        other => anyhow::bail!("unsupported exact timeline source collection {other}"),
    }
}

/// Reload an edge-only source through its collection at the pinned composite
/// commit. This retains the collection-version identity from the exact `_C`
/// commit and refuses declared signer metadata that does not match local
/// cryptographic verification.
async fn verified_exact_timeline_document_source(
    node: &defra_node::EmbeddedNode,
    identity: &identity::Did,
    collection: &str,
    expected: &crate::SignedDocumentVersionRef,
) -> Result<ExactDocumentSource> {
    let snapshot = verified_exact_document_snapshot_with_identity(
        node,
        collection,
        &expected.version,
        timeline_exact_source_selection(collection)?,
        Some(identity.clone()),
    )
    .await?;
    if snapshot.source != *expected {
        anyhow::bail!(
            "exact {collection} {} signer does not match its pinned timeline edge",
            expected.version.doc_id
        );
    }
    Ok(ExactDocumentSource::from(snapshot))
}

fn validated_rendered_provenance_manifest(
    rendered: &gents_protocol::row::RenderedRequestRow,
) -> Result<crate::rendered_request::ProvenanceManifest> {
    let raw = rendered
        .provenance_json
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "RenderedRequest {} omitted its provenance manifest",
                rendered.capture_key
            )
        })?;
    let value: Value = serde_json::from_str(raw).with_context(|| {
        format!(
            "decoding provenance manifest for RenderedRequest {}",
            rendered.capture_key
        )
    })?;
    let manifest: crate::rendered_request::ProvenanceManifest =
        serde_json::from_value(value.clone()).with_context(|| {
            format!(
                "decoding typed provenance manifest for RenderedRequest {}",
                rendered.capture_key
            )
        })?;
    if manifest.manifest_version != crate::rendered_request::PROVENANCE_MANIFEST_VERSION {
        anyhow::bail!(
            "RenderedRequest {} has unsupported provenance manifest version {}",
            rendered.capture_key,
            manifest.manifest_version
        );
    }
    if manifest.status != crate::rendered_request::ProvenanceStatus::CapturedOnly
        || manifest.capture_seam != crate::rendered_request::CaptureSeam::TransportBody
        || rendered.capture_scope.as_deref() != Some(manifest.capture_scope.as_str())
    {
        anyhow::bail!(
            "RenderedRequest {} provenance status/seam/scope disagrees with its durable capture",
            rendered.capture_key
        );
    }
    let canonical = serde_json::to_value(&manifest)
        .context("re-encoding rendered-request provenance manifest")?;
    if value != canonical {
        anyhow::bail!(
            "RenderedRequest {} provenance manifest is not canonical v{}",
            rendered.capture_key,
            crate::rendered_request::PROVENANCE_MANIFEST_VERSION
        );
    }

    let behavior_id = nonempty_owned(rendered.behavior_id.as_deref()).ok_or_else(|| {
        anyhow::anyhow!(
            "RenderedRequest {} provenance has no behavior id",
            rendered.capture_key
        )
    })?;
    let agent_did = nonempty_owned(rendered.agent_did.as_deref()).ok_or_else(|| {
        anyhow::anyhow!(
            "RenderedRequest {} provenance has no agent DID",
            rendered.capture_key
        )
    })?;
    crate::rendered_request::validate_transcript_snapshot(&manifest.transcript_snapshot)
        .context("validating rendered-request transcript snapshot")?;
    crate::rendered_request::validate_config_provenance(
        manifest.config_provenance_scope,
        manifest.config_provenance.as_ref(),
        &behavior_id,
        &agent_did,
    )
    .context("validating rendered-request config provenance")?;
    crate::rendered_request::validate_inference_call_provenance(
        manifest.inference_call_provenance_scope,
        manifest.inference_call_provenance.as_ref(),
    )
    .context("validating rendered-request inference-call provenance")?;

    let row_request = match (
        nonempty_owned(rendered.request_doc_id.as_deref()),
        nonempty_owned(rendered.request_source_commit_cid.as_deref()),
        nonempty_owned(rendered.request_source_signer_did.as_deref()),
        nonempty_owned(rendered.request_claim_commit_cid.as_deref()),
        nonempty_owned(rendered.request_claim_signer_did.as_deref()),
    ) {
        (None, None, None, None, None) => None,
        (
            Some(doc_id),
            Some(source_cid),
            Some(source_signer),
            Some(claim_cid),
            Some(claim_signer),
        ) => Some(crate::RequestExecutionProvenance::new(
            crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new(&doc_id, source_cid),
                source_signer,
            ),
            crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new(doc_id, claim_cid),
                claim_signer,
            ),
        )),
        _ => anyhow::bail!(
            "RenderedRequest {} has partial request source/claim provenance",
            rendered.capture_key
        ),
    };
    if manifest.request_provenance != row_request {
        anyhow::bail!(
            "RenderedRequest {} request columns disagree with its provenance manifest",
            rendered.capture_key
        );
    }
    if row_request.is_some()
        && (manifest.inference_call_provenance_scope
            != crate::rendered_request::InferenceCallProvenanceScope::AdmittedProviderCall
            || manifest.config_provenance_scope
                != crate::rendered_request::ConfigProvenanceScope::ReconciledDocumentRuntime)
    {
        anyhow::bail!(
            "document-backed RenderedRequest {} must pin admitted inference and reconciled config provenance",
            rendered.capture_key
        );
    }
    if let Some(provenance) = &manifest.request_provenance {
        provenance
            .validate_for_request(&provenance.source.version.doc_id, &agent_did)
            .context("validating rendered-request source/claim provenance")?;
    }

    let row_inference = match (
        nonempty_owned(rendered.inference_call_doc_id.as_deref()),
        nonempty_owned(rendered.inference_call_composite_commit_cid.as_deref()),
        nonempty_owned(rendered.inference_call_signer_did.as_deref()),
    ) {
        (None, None, None) => None,
        (Some(doc_id), Some(cid), Some(signer)) => Some(crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new(doc_id, cid),
            signer,
        )),
        _ => anyhow::bail!(
            "RenderedRequest {} has partial inference-call provenance",
            rendered.capture_key
        ),
    };
    if manifest.inference_call_provenance != row_inference {
        anyhow::bail!(
            "RenderedRequest {} inference-call columns disagree with its provenance manifest",
            rendered.capture_key
        );
    }

    Ok(manifest)
}

async fn verified_rendered_nested_source(
    node: &defra_node::EmbeddedNode,
    identity: &identity::Did,
    class: TimelineSourceClass,
    collection: &'static str,
    expected: &crate::SignedDocumentVersionRef,
    expected_collection_version_id: &str,
    expected_logical_field: Option<(&str, &str)>,
) -> Result<ExactTimelineSource> {
    let selection = match expected_logical_field {
        Some((field, _)) => field,
        None => timeline_exact_source_selection(collection)?,
    };
    let snapshot = verified_exact_document_snapshot_with_identity(
        node,
        collection,
        &expected.version,
        selection,
        Some(identity.clone()),
    )
    .await?;
    validate_rendered_nested_snapshot(
        collection,
        &snapshot,
        expected,
        expected_collection_version_id,
        expected_logical_field,
    )?;
    Ok(ExactTimelineSource {
        class,
        collection,
        source: ExactDocumentSource::from(snapshot),
    })
}

fn validate_rendered_nested_snapshot(
    collection: &str,
    snapshot: &VerifiedExactDocumentSnapshot,
    expected: &crate::SignedDocumentVersionRef,
    expected_collection_version_id: &str,
    expected_logical_field: Option<(&str, &str)>,
) -> Result<()> {
    if snapshot.source != *expected {
        anyhow::bail!(
            "exact {collection} {} signer does not match its rendered provenance reference",
            expected.version.doc_id
        );
    }
    if snapshot.collection_version_id != expected_collection_version_id {
        anyhow::bail!(
            "exact {collection} {} schema {} does not match rendered provenance schema {}",
            expected.version.doc_id,
            snapshot.collection_version_id,
            expected_collection_version_id
        );
    }
    if let Some((field, logical_id)) = expected_logical_field {
        if snapshot.document.get(field).and_then(Value::as_str) != Some(logical_id) {
            anyhow::bail!(
                "exact {collection} {} does not bind {field}={logical_id}",
                expected.version.doc_id
            );
        }
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct InferenceAdmissionSnapshot {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    behavior_id: String,
    agent_did: String,
    call_state: String,
}

async fn freeze_exact_timeline_rows(
    node: &defra_node::EmbeddedNode,
    rows: &mut RunTimelineRows,
    root_source: ExactDocumentSource,
) -> Result<RunTimelineSourceManifest> {
    let identity = timeline_reader_identity(node)?;
    if rows.request.doc_id.as_deref() != Some(root_source.exact.version.doc_id.as_str()) {
        anyhow::bail!(
            "exact timeline root document {} does not match decoded AgentRequest {:?}",
            root_source.exact.version.doc_id,
            rows.request.doc_id
        );
    }
    let coverage_gaps = open_timeline_coverage_gaps(rows, &root_source.exact);

    let mut sources = vec![ExactTimelineSource {
        class: TimelineSourceClass::Request,
        collection: "AgentRequest",
        source: root_source.clone(),
    }];
    let mut declared_edges = Vec::new();

    for request in &mut rows.requests {
        let doc_id = required_source_doc_id(
            "AgentRequest",
            request.request_id.as_str(),
            request.doc_id.as_deref(),
        )?;
        if doc_id == root_source.exact.version.doc_id {
            *request = rows.request.clone();
            continue;
        }
        let discovered_request_id = request.request_id.clone();
        let (exact_row, exact) = verified_current_exact_row::<TimelineRequestRow>(
            node,
            "AgentRequest",
            &doc_id,
            TIMELINE_REQUEST_SELECTION,
            &identity,
        )
        .await?;
        if exact_row.request_id != discovered_request_id {
            anyhow::bail!(
                "AgentRequest {doc_id} changed logical request id while freezing timeline provenance"
            );
        }
        *request = exact_row;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::Request,
            collection: "AgentRequest",
            source: exact,
        });
    }

    if let Some(session) = &mut rows.session {
        let doc_id = required_source_doc_id(
            "AgentSession",
            session.session_id.as_str(),
            session.doc_id.as_deref(),
        )?;
        let discovered_session_id = session.session_id.clone();
        let (exact_row, exact) = verified_current_exact_row::<TimelineSessionRow>(
            node,
            "AgentSession",
            &doc_id,
            TIMELINE_SESSION_SELECTION,
            &identity,
        )
        .await?;
        if exact_row.session_id != discovered_session_id {
            anyhow::bail!(
                "AgentSession {doc_id} changed logical session id while freezing timeline provenance"
            );
        }
        *session = exact_row;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::SessionProjection,
            collection: "AgentSession",
            source: exact,
        });
    }

    if let Some(conversation) = &mut rows.conversation {
        let doc_id = required_source_doc_id(
            "AgentConversation",
            conversation.session_id.as_str(),
            conversation.doc_id.as_deref(),
        )?;
        let discovered_session_id = conversation.session_id.clone();
        let (exact_row, exact) = verified_current_exact_row::<TimelineConversationRow>(
            node,
            "AgentConversation",
            &doc_id,
            TIMELINE_CONVERSATION_SELECTION,
            &identity,
        )
        .await?;
        if exact_row.session_id != discovered_session_id {
            anyhow::bail!(
                "AgentConversation {doc_id} changed logical session id while freezing timeline provenance"
            );
        }
        *conversation = exact_row;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::ConversationProjection,
            collection: "AgentConversation",
            source: exact,
        });
    }

    for message in &mut rows.messages {
        let label = format!("{}:{}", message.session_id, message.sequence);
        let doc_id = required_source_doc_id("AgentMessage", &label, message.doc_id.as_deref())?;
        let discovered = (
            message.session_id.clone(),
            message.request_id.clone(),
            message.request_doc_id.clone(),
            message.agent_did.clone(),
            message.sequence,
        );
        let (exact_row, exact) = verified_current_exact_row::<TimelineMessageRow>(
            node,
            "AgentMessage",
            &doc_id,
            TIMELINE_MESSAGE_SELECTION,
            &identity,
        )
        .await?;
        if (
            exact_row.session_id.clone(),
            exact_row.request_id.clone(),
            exact_row.request_doc_id.clone(),
            exact_row.agent_did.clone(),
            exact_row.sequence,
        ) != discovered
        {
            anyhow::bail!(
                "AgentMessage {doc_id} changed session/request/order while freezing timeline provenance"
            );
        }
        *message = exact_row;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::Message,
            collection: "AgentMessage",
            source: exact,
        });
    }

    for tool_call in &mut rows.tool_calls {
        let doc_id = required_source_doc_id(
            "AgentToolCall",
            tool_call.tool_call_id.as_str(),
            tool_call.doc_id.as_deref(),
        )?;
        let discovered = (
            tool_call.tool_call_id.clone(),
            tool_call.request_id.clone(),
            tool_call.session_id.clone(),
        );
        let (mut exact_row, exact) = verified_current_exact_row::<TimelineToolCallRow>(
            node,
            "AgentToolCall",
            &doc_id,
            TIMELINE_TOOL_CALL_SELECTION,
            &identity,
        )
        .await?;
        if (
            exact_row.tool_call_id.clone(),
            exact_row.request_id.clone(),
            exact_row.session_id.clone(),
        ) != discovered
        {
            anyhow::bail!(
                "AgentToolCall {doc_id} changed logical ownership while freezing timeline provenance"
            );
        }
        validate_exact_tool_fact_edges(&exact_row, &exact.exact, tool_call)?;
        exact_row.result_fact = tool_call.result_fact.clone();
        exact_row.omission_fact = tool_call.omission_fact.clone();
        exact_row.approval_fact = tool_call.approval_fact.clone();
        if let Some(fact) = &exact_row.result_fact {
            let result_source = verified_exact_timeline_document_source(
                node,
                &identity,
                "AgentToolResult",
                &crate::SignedDocumentVersionRef::new(
                    crate::DocumentVersionRef::new(&fact.doc_id, &fact.composite_commit_cid),
                    &fact.signer_did,
                ),
            )
            .await?;
            let result_source = ExactTimelineSource {
                class: TimelineSourceClass::ToolResult,
                collection: "AgentToolResult",
                source: result_source,
            };
            declared_edges.push(result_source.declared_edge());
            sources.push(result_source);
            let historical_call = crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new(
                    &fact.tool_call_doc_id,
                    &fact.tool_call_composite_commit_cid,
                ),
                &fact.tool_call_signer_did,
            );
            let historical_call_source = ExactTimelineSource {
                class: TimelineSourceClass::ToolCall,
                collection: "AgentToolCall",
                source: verified_exact_timeline_document_source(
                    node,
                    &identity,
                    "AgentToolCall",
                    &historical_call,
                )
                .await?,
            };
            declared_edges.push(historical_call_source.declared_edge());
            sources.push(historical_call_source);
        }
        if let Some(fact) = &exact_row.omission_fact {
            let omission_source = verified_exact_timeline_document_source(
                node,
                &identity,
                "AgentToolOutputOmission",
                &crate::SignedDocumentVersionRef::new(
                    crate::DocumentVersionRef::new(&fact.doc_id, &fact.composite_commit_cid),
                    &fact.signer_did,
                ),
            )
            .await?;
            let omission_source = ExactTimelineSource {
                class: TimelineSourceClass::ToolOutputOmission,
                collection: "AgentToolOutputOmission",
                source: omission_source,
            };
            declared_edges.push(omission_source.declared_edge());
            sources.push(omission_source);
            let historical_call = crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new(
                    &fact.tool_call_doc_id,
                    &fact.tool_call_composite_commit_cid,
                ),
                &fact.tool_call_signer_did,
            );
            let historical_call_source = ExactTimelineSource {
                class: TimelineSourceClass::ToolCall,
                collection: "AgentToolCall",
                source: verified_exact_timeline_document_source(
                    node,
                    &identity,
                    "AgentToolCall",
                    &historical_call,
                )
                .await?,
            };
            declared_edges.push(historical_call_source.declared_edge());
            sources.push(historical_call_source);
        }
        if let Some(fact) = &exact_row.approval_fact {
            let approval_source = verified_exact_timeline_document_source(
                node,
                &identity,
                "AgentToolApproval",
                &crate::SignedDocumentVersionRef::new(
                    crate::DocumentVersionRef::new(&fact.doc_id, &fact.composite_commit_cid),
                    &fact.signer_did,
                ),
            )
            .await?;
            let approval_source = ExactTimelineSource {
                class: TimelineSourceClass::ToolApproval,
                collection: "AgentToolApproval",
                source: approval_source,
            };
            declared_edges.push(approval_source.declared_edge());
            sources.push(approval_source);
            let historical_call = crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new(
                    &fact.tool_call_doc_id,
                    &fact.tool_call_composite_commit_cid,
                ),
                &fact.tool_call_signer_did,
            );
            let historical_call_source = ExactTimelineSource {
                class: TimelineSourceClass::ToolCall,
                collection: "AgentToolCall",
                source: verified_exact_timeline_document_source(
                    node,
                    &identity,
                    "AgentToolCall",
                    &historical_call,
                )
                .await?,
            };
            declared_edges.push(historical_call_source.declared_edge());
            sources.push(historical_call_source);
        }
        *tool_call = exact_row;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::ToolCall,
            collection: "AgentToolCall",
            source: exact,
        });
    }

    for response in &mut rows.responses {
        let doc_id = required_source_doc_id(
            "AgentResponse",
            response.request_id.as_str(),
            response.doc_id.as_deref(),
        )?;
        let discovered_request_id = response.request_id.clone();
        let (exact_row, exact) = verified_current_exact_row::<TimelineResponseRow>(
            node,
            "AgentResponse",
            &doc_id,
            TIMELINE_RESPONSE_SELECTION,
            &identity,
        )
        .await?;
        if exact_row.request_id != discovered_request_id {
            anyhow::bail!(
                "AgentResponse {doc_id} changed request id while freezing timeline provenance"
            );
        }
        if exact_row
            .agent_did
            .as_deref()
            .is_some_and(|agent_did| agent_did != exact.exact.signer_did)
        {
            anyhow::bail!(
                "AgentResponse {doc_id} signer {} does not match agent {:?}",
                exact.exact.signer_did,
                exact_row.agent_did
            );
        }
        *response = exact_row;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::ResponseLive,
            collection: "AgentResponse",
            source: exact,
        });
    }

    for call in &mut rows.inference_calls {
        let label = format!("{}:{}:{}", call.request_id, call.call_seq, call.attempt);
        let doc_id = required_source_doc_id("InferenceCall", &label, call.doc_id.as_deref())?;
        let discovered = (
            call.call_id.clone(),
            call.request_id.clone(),
            call.call_seq,
            call.attempt,
        );
        let (exact_row, exact) = verified_current_exact_row::<TimelineInferenceCallRow>(
            node,
            "InferenceCall",
            &doc_id,
            TIMELINE_INFERENCE_CALL_SELECTION,
            &identity,
        )
        .await?;
        if (
            exact_row.call_id.clone(),
            exact_row.request_id.clone(),
            exact_row.call_seq,
            exact_row.attempt,
        ) != discovered
        {
            anyhow::bail!(
                "InferenceCall {doc_id} changed logical call identity while freezing timeline provenance"
            );
        }
        *call = exact_row;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::InferenceCall,
            collection: "InferenceCall",
            source: exact,
        });
    }

    let rendered_sources = exactify_rendered_requests(node, &mut rows.rendered_requests).await?;
    for (rendered, source) in rows.rendered_requests.iter().zip(rendered_sources) {
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::RenderedRequest,
            collection: "RenderedRequest",
            source,
        });
        for source in verified_rendered_request_edge_sources(node, &identity, &rendered.row).await?
        {
            declared_edges.push(source.declared_edge());
            sources.push(source);
        }
    }

    let outcome_sources = exactify_response_outcomes(node, &mut rows.response_outcomes).await?;
    for (outcome, source) in rows.response_outcomes.iter().zip(outcome_sources) {
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::ResponseOutcome,
            collection: "AgentResponseOutcome",
            source,
        });
        for source in verified_response_outcome_edge_sources(node, &identity, &outcome.row).await? {
            declared_edges.push(source.declared_edge());
            sources.push(source);
        }
    }

    let compaction_sources =
        exactify_compaction_entries(node, &mut rows.compaction_entries).await?;
    for (compaction, source) in rows.compaction_entries.iter().zip(compaction_sources) {
        let exact = compaction
            .exact
            .clone()
            .context("exact CompactionEntry enrichment omitted source")?;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::Compaction,
            collection: "CompactionEntry",
            source,
        });
        for source in
            verified_compaction_edge_sources(node, &identity, &compaction.row, &exact).await?
        {
            declared_edges.push(source.declared_edge());
            sources.push(source);
        }
    }

    freeze_exact_timeline_source_manifest(root_source, sources, &coverage_gaps, &declared_edges)
}

fn validate_exact_tool_fact_edges(
    exact_row: &TimelineToolCallRow,
    exact: &crate::SignedDocumentVersionRef,
    discovered: &TimelineToolCallRow,
) -> Result<()> {
    let result_edge = complete_edge_doc_id(
        exact_row.result_doc_id.as_deref(),
        exact_row.result_composite_commit_cid.as_deref(),
        exact_row.result_signer_did.as_deref(),
        "exact AgentToolCall result",
    )?;
    match (result_edge, discovered.result_fact.as_ref()) {
        (None, None) => {}
        (Some(result_doc_id), Some(fact))
            if fact.doc_id == result_doc_id
                && exact_row.result_composite_commit_cid.as_deref()
                    == Some(fact.composite_commit_cid.as_str())
                && exact_row.result_signer_did.as_deref() == Some(fact.signer_did.as_str()) => {}
        _ => anyhow::bail!(
            "AgentToolCall {} exact result edge changed after fact verification",
            exact.version.doc_id
        ),
    }

    let omission_edge = complete_edge_doc_id(
        exact_row.omission_doc_id.as_deref(),
        exact_row.omission_composite_commit_cid.as_deref(),
        exact_row.omission_signer_did.as_deref(),
        "exact AgentToolCall omission",
    )?;
    match (omission_edge, discovered.omission_fact.as_ref()) {
        (None, None) => {}
        (Some(omission_doc_id), Some(fact))
            if fact.doc_id == omission_doc_id
                && exact_row.omission_composite_commit_cid.as_deref()
                    == Some(fact.composite_commit_cid.as_str())
                && exact_row.omission_signer_did.as_deref() == Some(fact.signer_did.as_str()) => {}
        _ => anyhow::bail!(
            "AgentToolCall {} exact omission edge changed after fact verification",
            exact.version.doc_id
        ),
    }
    validate_terminal_outcome_edge_shape(exact_row)?;

    let approval_edge = complete_edge_doc_id(
        exact_row.approval_doc_id.as_deref(),
        exact_row.approval_composite_commit_cid.as_deref(),
        exact_row.approval_signer_did.as_deref(),
        "exact AgentToolCall approval",
    )?;
    match (approval_edge, discovered.approval_fact.as_ref()) {
        (None, None) => {}
        (Some(approval_doc_id), Some(fact))
            if fact.doc_id == approval_doc_id
                && exact_row.approval_composite_commit_cid.as_deref()
                    == Some(fact.composite_commit_cid.as_str())
                && exact_row.approval_signer_did.as_deref() == Some(fact.signer_did.as_str()) => {}
        _ => anyhow::bail!(
            "AgentToolCall {} exact approval edge changed after fact verification",
            exact.version.doc_id
        ),
    }
    Ok(())
}

fn freeze_exact_timeline_source_manifest(
    root: ExactDocumentSource,
    mut sources: Vec<ExactTimelineSource>,
    coverage_gaps: &[TimelineCoverageGap],
    declared_edges: &[TimelineDeclaredExactEdge],
) -> Result<RunTimelineSourceManifest> {
    sources.sort_by(|left, right| {
        left.class
            .cmp(&right.class)
            .then_with(|| left.collection.cmp(right.collection))
            .then_with(|| {
                left.source
                    .collection_version_id
                    .cmp(&right.source.collection_version_id)
            })
            .then_with(|| {
                left.source
                    .exact
                    .version
                    .doc_id
                    .cmp(&right.source.exact.version.doc_id)
            })
            .then_with(|| {
                left.source
                    .exact
                    .version
                    .composite_commit_cid
                    .cmp(&right.source.exact.version.composite_commit_cid)
            })
            .then_with(|| {
                left.source
                    .exact
                    .signer_did
                    .cmp(&right.source.exact.signer_did)
            })
    });
    sources.dedup_by(|left, right| {
        left.class == right.class
            && left.collection == right.collection
            && left.source == right.source
    });

    let root_position = sources
        .iter()
        .position(|source| {
            source.class == TimelineSourceClass::Request
                && source.collection == "AgentRequest"
                && source.source == root
        })
        .context("complete timeline sources omitted the exact request root")?;
    let root_source = sources.remove(root_position);
    sources.insert(0, root_source);

    let mut ordinals = std::collections::BTreeMap::<TimelineSourceClass, u32>::new();
    let mut expected = Vec::with_capacity(sources.len());
    let mut observed = Vec::with_capacity(sources.len());
    let mut decisions = Vec::with_capacity(sources.len());
    for source in sources {
        let ordinal = ordinals.entry(source.class).or_default();
        let slot = TimelineSourceSlot::new(source.class, *ordinal);
        *ordinal = ordinal
            .checked_add(1)
            .context("timeline source ordinal overflow")?;
        expected.push(TimelineExpectedSlot {
            slot: slot.clone(),
            requirement: TimelineSlotRequirement::Required,
        });
        observed.push(TimelineObservedSource {
            slot: slot.clone(),
            collection: source.collection.to_string(),
            collection_version_id: source.source.collection_version_id.clone(),
            exact: source.source.exact.clone(),
        });
        decisions.push(TimelineSourceDecision::Include {
            slot,
            collection: source.collection.to_string(),
            collection_version_id: source.source.collection_version_id,
            exact: source.source.exact,
        });
    }
    freeze_timeline_manifest_with_declared_edges(
        &TimelineRootSelector::Exact(root.exact.clone()),
        &[TimelineRootCandidate {
            request_id: String::new(),
            exact: root.exact,
            current_head_count: 1,
        }],
        &expected,
        &observed,
        &decisions,
        coverage_gaps,
        declared_edges,
    )
    .map_err(Into::into)
}

/// Describe the source domains that the current loader discovers through
/// mutable logical/session scans. Every returned row is exact-reloaded and
/// signature-verified later, but these gaps remain until a durable head,
/// cardinality fact, or embedded source manifest independently closes the
/// corresponding domain.
fn open_timeline_coverage_gaps(
    rows: &RunTimelineRows,
    root: &crate::SignedDocumentVersionRef,
) -> Vec<TimelineCoverageGap> {
    let mut gaps = BTreeSet::new();
    let mut insert = |kind, source_class, collection: &str, scope_id: String| {
        gaps.insert(TimelineCoverageGap {
            kind,
            source_class,
            collection: collection.to_string(),
            scope_id,
        });
    };

    insert(
        TimelineCoverageGapKind::NonAtomicObservation,
        TimelineSourceClass::Request,
        "AgentRequest",
        root.version.doc_id.clone(),
    );
    insert(
        TimelineCoverageGapKind::OpenLogicalExtent,
        TimelineSourceClass::Request,
        "AgentRequest",
        format!("children:{}", rows.request.request_id),
    );

    let request_ids = timeline_request_ids(&rows.requests);
    for request_id in request_ids {
        for (source_class, collection) in [
            (TimelineSourceClass::InferenceCall, "InferenceCall"),
            (TimelineSourceClass::RenderedRequest, "RenderedRequest"),
        ] {
            insert(
                TimelineCoverageGapKind::OpenLogicalExtent,
                source_class,
                collection,
                request_id.clone(),
            );
        }
    }
    for request_doc_id in rows
        .requests
        .iter()
        .filter_map(|request| request.doc_id.as_deref())
    {
        insert(
            TimelineCoverageGapKind::OpenLogicalExtent,
            TimelineSourceClass::ResponseOutcome,
            "AgentResponseOutcome",
            request_doc_id.to_string(),
        );
    }

    let session_ids = timeline_session_ids(&rows.requests);
    for session_id in &session_ids {
        for (source_class, collection) in [
            (TimelineSourceClass::Request, "AgentRequest"),
            (TimelineSourceClass::SessionProjection, "AgentSession"),
            (
                TimelineSourceClass::ConversationProjection,
                "AgentConversation",
            ),
            (TimelineSourceClass::Message, "AgentMessage"),
            (TimelineSourceClass::ToolCall, "AgentToolCall"),
            (TimelineSourceClass::ResponseLive, "AgentResponse"),
            (TimelineSourceClass::Compaction, "CompactionEntry"),
        ] {
            insert(
                TimelineCoverageGapKind::OpenSessionExtent,
                source_class,
                collection,
                session_id.clone(),
            );
        }
    }
    if session_ids.is_empty() || rows.request.session_id.is_none() {
        insert(
            TimelineCoverageGapKind::OpenLogicalExtent,
            TimelineSourceClass::ResponseLive,
            "AgentResponse",
            rows.request.request_id.clone(),
        );
    }

    gaps.into_iter().collect()
}

fn required_source_doc_id(collection: &str, label: &str, doc_id: Option<&str>) -> Result<String> {
    doc_id
        .map(str::trim)
        .filter(|doc_id| !doc_id.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{collection} {label} omitted _docID"))
}

async fn verified_rendered_request_edge_sources(
    node: &defra_node::EmbeddedNode,
    identity: &identity::Did,
    rendered: &gents_protocol::row::RenderedRequestRow,
) -> Result<Vec<ExactTimelineSource>> {
    let manifest = validated_rendered_provenance_manifest(rendered)?;
    let mut sources = Vec::new();
    if let Some(provenance) = &manifest.request_provenance {
        let request_doc_id = &provenance.source.version.doc_id;
        let agent_did = nonempty_owned(rendered.agent_did.as_deref()).ok_or_else(|| {
            anyhow::anyhow!(
                "RenderedRequest {} has request provenance but no agent DID",
                rendered.capture_key
            )
        })?;
        let claim = crate::lifecycle::verify_persisted_execution_provenance(
            node,
            identity,
            provenance,
            request_doc_id,
            &agent_did,
        )
        .await
        .with_context(|| {
            format!(
                "verifying request source/claim edges for RenderedRequest {}",
                rendered.capture_key
            )
        })?;
        if rendered.request_id.as_deref() != Some(claim.request_id.as_str())
            || rendered.session_id.as_deref() != Some(claim.session_id.as_str())
            || rendered.behavior_id.as_deref() != claim.behavior_id.as_deref()
        {
            anyhow::bail!(
                "RenderedRequest {} identity fields disagree with its verified request claim",
                rendered.capture_key
            );
        }
        let request_source = verified_exact_timeline_document_source(
            node,
            identity,
            "AgentRequest",
            &provenance.source,
        )
        .await?;
        let request_claim = verified_exact_timeline_document_source(
            node,
            identity,
            "AgentRequest",
            &provenance.claim,
        )
        .await?;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::Request,
            collection: "AgentRequest",
            source: request_source,
        });
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::Request,
            collection: "AgentRequest",
            source: request_claim,
        });
    }

    if let Some(inference_provenance) = &manifest.inference_call_provenance {
        let doc_id = &inference_provenance.version.doc_id;
        let snapshot = verified_exact_document_snapshot_with_identity(
            node,
            "InferenceCall",
            &inference_provenance.version,
            TIMELINE_INFERENCE_ADMISSION_SELECTION,
            Some(identity.clone()),
        )
        .await
        .with_context(|| {
            format!(
                "verifying inference admission edge for RenderedRequest {}",
                rendered.capture_key
            )
        })?;
        let admission = snapshot.decode::<InferenceAdmissionSnapshot>()?;
        if admission.doc_id != *doc_id
            || rendered.request_id.as_deref() != Some(admission.request_id.as_str())
            || rendered.behavior_id.as_deref() != Some(admission.behavior_id.as_str())
            || rendered.agent_did.as_deref() != Some(admission.agent_did.as_str())
            || admission.call_state != "running"
            || snapshot.source != *inference_provenance
            || snapshot.source.signer_did != admission.agent_did
        {
            anyhow::bail!(
                    "RenderedRequest {} inference admission edge failed exact identity/state verification",
                    rendered.capture_key
                );
        }
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::InferenceCall,
            collection: "InferenceCall",
            source: ExactDocumentSource::from(snapshot),
        });
    }

    let rendered_session_id = nonempty_owned(rendered.session_id.as_deref());
    for fact_ref in &manifest.transcript_snapshot {
        let expected = crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new(&fact_ref.doc_id, &fact_ref.composite_commit_cid),
            &fact_ref.signer_did,
        );
        let snapshot = verified_exact_document_snapshot_with_identity(
            node,
            "AgentMessage",
            &expected.version,
            "session_id sequence",
            Some(identity.clone()),
        )
        .await
        .with_context(|| {
            format!(
                "verifying transcript source {} for RenderedRequest {}",
                fact_ref.doc_id, rendered.capture_key
            )
        })?;
        validate_rendered_nested_snapshot(
            "AgentMessage",
            &snapshot,
            &expected,
            &fact_ref.collection_version_id,
            None,
        )?;
        if snapshot.document.get("sequence").and_then(Value::as_u64)
            != Some(u64::from(fact_ref.sequence))
            || snapshot.document.get("session_id").and_then(Value::as_str)
                != rendered_session_id.as_deref()
        {
            anyhow::bail!(
                "RenderedRequest {} transcript source {} failed exact identity/session/sequence verification",
                rendered.capture_key,
                fact_ref.doc_id
            );
        }
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::Message,
            collection: "AgentMessage",
            source: ExactDocumentSource::from(snapshot),
        });
    }

    if let Some(config) = &manifest.config_provenance {
        let mut config_refs = vec![
            (&config.principal, "AgentPrincipal"),
            (&config.behavior, "AgentBehavior"),
            (&config.inference_backend, "InferenceBackend"),
            (&config.inference_profile, "InferenceProfile"),
        ];
        if let Some(tool_selection) = &config.tool_selection {
            config_refs.push((tool_selection, "ToolSelection"));
        }
        config_refs.extend(
            config
                .datastore_tool_surfaces
                .iter()
                .map(|surface| (surface, "DatastoreToolSurface")),
        );
        config_refs.extend(config.skills.iter().map(|skill| (skill, "Skill")));
        for (fact_ref, expected_collection) in config_refs {
            if fact_ref.collection != expected_collection {
                anyhow::bail!(
                    "RenderedRequest {} config reference declared collection {} in {expected_collection} position",
                    rendered.capture_key,
                    fact_ref.collection
                );
            }
            let logical_field = timeline_exact_source_selection(expected_collection)?;
            let source = verified_rendered_nested_source(
                node,
                identity,
                TimelineSourceClass::ResolvedConfig,
                expected_collection,
                &fact_ref.source,
                &fact_ref.collection_version_id,
                Some((logical_field, &fact_ref.logical_id)),
            )
            .await
            .with_context(|| {
                format!(
                    "verifying {expected_collection} config source {} for RenderedRequest {}",
                    fact_ref.source.version.doc_id, rendered.capture_key
                )
            })?;
            sources.push(source);
        }
    }
    Ok(sources)
}

async fn verified_response_outcome_edge_sources(
    node: &defra_node::EmbeddedNode,
    identity: &identity::Did,
    outcome: &gents_protocol::row::AgentResponseOutcomeRow,
) -> Result<Vec<ExactTimelineSource>> {
    let kind = crate::response_outcome::validate_timeline_response_outcome_row(outcome)
        .context("validating AgentResponseOutcome structural contract")?;
    let agent_did = nonempty_owned(outcome.agent_did.as_deref()).ok_or_else(|| {
        anyhow::anyhow!(
            "AgentResponseOutcome for request {} has no agent DID",
            outcome.request_doc_id
        )
    })?;
    let source_cid = nonempty_owned(outcome.request_source_composite_commit_cid.as_deref())
        .context("AgentResponseOutcome request source CID is missing")?;
    let source_signer = nonempty_owned(outcome.request_source_signer_did.as_deref())
        .context("AgentResponseOutcome request source signer is missing")?;
    let claim_cid = nonempty_owned(outcome.request_claim_composite_commit_cid.as_deref())
        .context("AgentResponseOutcome request claim CID is missing")?;
    let claim_signer = nonempty_owned(outcome.request_claim_signer_did.as_deref())
        .context("AgentResponseOutcome request claim signer is missing")?;
    let provenance = crate::RequestExecutionProvenance::new(
        crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new(&outcome.request_doc_id, &source_cid),
            &source_signer,
        ),
        crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new(&outcome.request_doc_id, &claim_cid),
            &claim_signer,
        ),
    );
    let claim = crate::lifecycle::verify_persisted_execution_provenance(
        node,
        identity,
        &provenance,
        &outcome.request_doc_id,
        &agent_did,
    )
    .await
    .context("verifying AgentResponseOutcome request source/claim provenance")?;
    if outcome.request_id.as_deref() != Some(claim.request_id.as_str())
        || outcome.session_id.as_deref() != Some(claim.session_id.as_str())
        || outcome.behavior_id.as_deref() != claim.behavior_id.as_deref()
        || outcome
            .requester_did
            .as_deref()
            .filter(|did| !did.trim().is_empty())
            != claim
                .requester_did
                .as_deref()
                .filter(|did| !did.trim().is_empty())
    {
        anyhow::bail!(
            "AgentResponseOutcome for request {} disagrees with its exact claim identity",
            outcome.request_doc_id
        );
    }

    let request_source =
        verified_exact_timeline_document_source(node, identity, "AgentRequest", &provenance.source)
            .await?;
    let request_claim =
        verified_exact_timeline_document_source(node, identity, "AgentRequest", &provenance.claim)
            .await?;
    let mut sources = vec![
        ExactTimelineSource {
            class: TimelineSourceClass::Request,
            collection: "AgentRequest",
            source: request_source,
        },
        ExactTimelineSource {
            class: TimelineSourceClass::Request,
            collection: "AgentRequest",
            source: request_claim,
        },
    ];
    match (
        nonempty_owned(outcome.final_message_doc_id.as_deref()),
        nonempty_owned(outcome.final_message_composite_commit_cid.as_deref()),
        nonempty_owned(outcome.final_message_collection_version_id.as_deref()),
        nonempty_owned(outcome.final_message_signer_did.as_deref()),
        outcome.final_message_sequence,
    ) {
        (None, None, None, None, None) => {
            if kind == crate::response_outcome::ResponseOutcomeKind::Complete {
                anyhow::bail!(
                    "complete AgentResponseOutcome for request {} has no final message fact",
                    outcome.request_doc_id
                );
            }
        }
        (
            Some(doc_id),
            Some(cid),
            Some(declared_collection_version_id),
            Some(declared_signer),
            Some(sequence),
        ) => {
            let version = crate::DocumentVersionRef::new(&doc_id, &cid);
            let snapshot = verified_exact_document_snapshot_with_identity(
                node,
                "AgentMessage",
                &version,
                TIMELINE_MESSAGE_SELECTION,
                Some(identity.clone()),
            )
            .await
            .context("verifying AgentResponseOutcome final message fact")?;
            let message = snapshot.decode::<TimelineMessageRow>()?;
            if message.doc_id.as_deref() != Some(doc_id.as_str())
                || message.sequence != sequence
                || message.role != "assistant"
                || outcome.request_id.as_deref() != message.request_id.as_deref()
                || message.request_doc_id.as_deref() != Some(outcome.request_doc_id.as_str())
                || message.agent_did.as_deref() != Some(agent_did.as_str())
                || outcome.session_id.as_deref() != Some(message.session_id.as_str())
                || snapshot.collection_version_id != declared_collection_version_id
                || snapshot.source.signer_did != declared_signer
                || snapshot.source.signer_did != agent_did
            {
                anyhow::bail!(
                    "AgentResponseOutcome for request {} final message failed exact identity/signature verification",
                    outcome.request_doc_id
                );
            }
            sources.push(ExactTimelineSource {
                class: TimelineSourceClass::Message,
                collection: "AgentMessage",
                source: ExactDocumentSource::from(snapshot),
            });
        }
        _ => anyhow::bail!(
            "AgentResponseOutcome for request {} has a partial final-message reference",
            outcome.request_doc_id
        ),
    }
    Ok(sources)
}

async fn verified_compaction_edge_sources(
    node: &defra_node::EmbeddedNode,
    identity: &identity::Did,
    row: &gents_protocol::row::CompactionEntryRow,
    exact: &crate::SignedDocumentVersionRef,
) -> Result<Vec<ExactTimelineSource>> {
    let manifest_version = row
        .source_manifest_version
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "CompactionEntry {} has no valid source manifest version",
                row.compaction_key
            )
        })?;
    let manifest_json = nonempty_owned(row.source_manifest_json.as_deref()).ok_or_else(|| {
        anyhow::anyhow!(
            "CompactionEntry {} has no source manifest JSON",
            row.compaction_key
        )
    })?;
    let manifest: crate::session::CompactionSourceManifest = serde_json::from_str(&manifest_json)
        .with_context(|| {
        format!(
            "decoding CompactionEntry {} source manifest",
            row.compaction_key
        )
    })?;
    if manifest.manifest_version != manifest_version {
        anyhow::bail!(
            "CompactionEntry {} source manifest version {} disagrees with JSON version {}",
            row.compaction_key,
            manifest_version,
            manifest.manifest_version
        );
    }
    let canonical =
        crate::rendered_request::canonical_json_string(&serde_json::to_value(&manifest)?)?;
    if canonical != manifest_json {
        anyhow::bail!(
            "CompactionEntry {} source manifest is not canonical",
            row.compaction_key
        );
    }
    crate::session::verify_compaction_entry_for_timeline(node, row, exact)
        .await
        .with_context(|| {
            format!(
                "verifying CompactionEntry {} exact source graph",
                row.compaction_key
            )
        })?;

    let mut sources = Vec::new();
    for fact in &manifest.transcript_snapshot {
        let exact = crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new(&fact.doc_id, &fact.composite_commit_cid),
            &fact.signer_did,
        );
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::Message,
            collection: "AgentMessage",
            source: verified_exact_timeline_document_source(node, identity, "AgentMessage", &exact)
                .await?,
        });
    }
    for fact in resolved_config_fact_refs(&manifest.config_provenance) {
        let collection = timeline_config_collection(&fact.collection)?;
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::ResolvedConfig,
            collection,
            source: verified_exact_timeline_document_source(
                node,
                identity,
                collection,
                &fact.source,
            )
            .await?,
        });
    }
    for fact in &manifest.prior_compactions {
        sources.push(ExactTimelineSource {
            class: TimelineSourceClass::Compaction,
            collection: "CompactionEntry",
            source: verified_exact_timeline_document_source(
                node,
                identity,
                "CompactionEntry",
                &fact.source,
            )
            .await?,
        });
    }

    match (
        nonempty_owned(row.fork_source_doc_id.as_deref()),
        nonempty_owned(row.fork_source_composite_commit_cid.as_deref()),
        nonempty_owned(row.fork_source_signer_did.as_deref()),
    ) {
        (None, None, None) => {}
        (Some(doc_id), Some(cid), Some(declared_signer)) => {
            let version = crate::DocumentVersionRef::new(&doc_id, &cid);
            let snapshot = verified_exact_document_snapshot_with_identity(
                node,
                "CompactionEntry",
                &version,
                TIMELINE_COMPACTION_SELECTION,
                Some(identity.clone()),
            )
            .await
            .context("verifying CompactionEntry fork source")?;
            if snapshot.source.signer_did != declared_signer {
                anyhow::bail!(
                    "CompactionEntry {} fork source signer does not match exact block signer",
                    row.compaction_key
                );
            }
            sources.push(ExactTimelineSource {
                class: TimelineSourceClass::Compaction,
                collection: "CompactionEntry",
                source: ExactDocumentSource::from(snapshot),
            });
        }
        _ => anyhow::bail!(
            "CompactionEntry {} has a partial fork source reference",
            row.compaction_key
        ),
    }
    Ok(sources)
}

fn resolved_config_fact_refs(
    provenance: &crate::ResolvedBehaviorConfigProvenance,
) -> Vec<&crate::ConfigFactRef> {
    let mut facts = vec![
        &provenance.principal,
        &provenance.behavior,
        &provenance.inference_backend,
        &provenance.inference_profile,
    ];
    if let Some(tool_selection) = provenance.tool_selection.as_ref() {
        facts.push(tool_selection);
    }
    facts.extend(provenance.datastore_tool_surfaces.iter());
    facts.extend(provenance.skills.iter());
    facts
}

fn timeline_config_collection(collection: &str) -> Result<&'static str> {
    match collection {
        "AgentPrincipal" => Ok("AgentPrincipal"),
        "AgentBehavior" => Ok("AgentBehavior"),
        "InferenceBackend" => Ok("InferenceBackend"),
        "InferenceProfile" => Ok("InferenceProfile"),
        "ToolSelection" => Ok("ToolSelection"),
        "DatastoreToolSurface" => Ok("DatastoreToolSurface"),
        "Skill" => Ok("Skill"),
        other => anyhow::bail!("unsupported timeline config source collection {other}"),
    }
}

fn nonempty_owned(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

async fn exactify_rendered_requests(
    node: &defra_node::EmbeddedNode,
    rows: &mut [TimelineRenderedRequestRow],
) -> Result<Vec<ExactDocumentSource>> {
    let identity = timeline_reader_identity(node)?;
    let mut sources = Vec::with_capacity(rows.len());
    for rendered in rows {
        let discovered_doc_id = rendered
            .row
            .doc_id
            .as_deref()
            .filter(|doc_id| !doc_id.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "RenderedRequest {} omitted _docID",
                    rendered.row.capture_key
                )
            })?;
        let discovered_capture_key = rendered.row.capture_key.clone();
        let (exact_row, exact) =
            verified_current_exact_row::<gents_protocol::row::RenderedRequestRow>(
                node,
                "RenderedRequest",
                discovered_doc_id,
                TIMELINE_RENDERED_REQUEST_SELECTION,
                &identity,
            )
            .await?;
        if exact_row.capture_key != discovered_capture_key {
            anyhow::bail!(
                "RenderedRequest {discovered_doc_id} changed capture key while freezing timeline provenance"
            );
        }
        if exact_row
            .agent_did
            .as_deref()
            .is_some_and(|agent_did| agent_did != exact.exact.signer_did)
        {
            anyhow::bail!(
                "RenderedRequest {discovered_doc_id} signer {} does not match agent {:?}",
                exact.exact.signer_did,
                exact_row.agent_did
            );
        }
        let field = document_field_version_ref_with_identity(
            node,
            "RenderedRequest",
            &exact.exact.version,
            "request_json",
            Some(identity.clone()),
        )
        .await?;
        rendered.row = exact_row;
        rendered.exact = Some(exact.exact.clone());
        rendered.request_json_field_cid = Some(field.field_commit_cid);
        sources.push(exact);
    }
    Ok(sources)
}

async fn exactify_response_outcomes(
    node: &defra_node::EmbeddedNode,
    rows: &mut [TimelineResponseOutcomeRow],
) -> Result<Vec<ExactDocumentSource>> {
    let identity = timeline_reader_identity(node)?;
    let mut sources = Vec::with_capacity(rows.len());
    for outcome in rows {
        let discovered_doc_id = outcome
            .row
            .doc_id
            .as_deref()
            .filter(|doc_id| !doc_id.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "AgentResponseOutcome for request {} omitted _docID",
                    outcome.row.request_doc_id
                )
            })?;
        let discovered_request_doc_id = outcome.row.request_doc_id.clone();
        let (exact_row, exact) =
            verified_current_exact_row::<gents_protocol::row::AgentResponseOutcomeRow>(
                node,
                "AgentResponseOutcome",
                discovered_doc_id,
                TIMELINE_RESPONSE_OUTCOME_SELECTION,
                &identity,
            )
            .await?;
        if exact_row.request_doc_id != discovered_request_doc_id {
            anyhow::bail!(
                "AgentResponseOutcome {discovered_doc_id} changed request document while freezing timeline provenance"
            );
        }
        let agent_did = exact_row
            .agent_did
            .as_deref()
            .filter(|agent_did| !agent_did.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("AgentResponseOutcome {discovered_doc_id} has no agent DID")
            })?;
        if agent_did != exact.exact.signer_did {
            anyhow::bail!(
                "AgentResponseOutcome {discovered_doc_id} signer {} does not match agent {agent_did}",
                exact.exact.signer_did
            );
        }
        outcome.row = exact_row;
        outcome.exact = Some(exact.exact.clone());
        sources.push(exact);
    }
    Ok(sources)
}

async fn exactify_compaction_entries(
    node: &defra_node::EmbeddedNode,
    rows: &mut [TimelineCompactionEntryRow],
) -> Result<Vec<ExactDocumentSource>> {
    let identity = timeline_reader_identity(node)?;
    let mut sources = Vec::with_capacity(rows.len());
    for compaction in rows {
        let discovered_doc_id = compaction
            .row
            .doc_id
            .as_deref()
            .filter(|doc_id| !doc_id.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "CompactionEntry {} omitted _docID",
                    compaction.row.compaction_key
                )
            })?;
        let discovered_key = compaction.row.compaction_key.clone();
        let discovered_sequence = compaction.row.sequence;
        let (exact_row, exact) =
            verified_current_exact_row::<gents_protocol::row::CompactionEntryRow>(
                node,
                "CompactionEntry",
                discovered_doc_id,
                TIMELINE_COMPACTION_SELECTION,
                &identity,
            )
            .await?;
        if exact_row.compaction_key != discovered_key || exact_row.sequence != discovered_sequence {
            anyhow::bail!(
                "CompactionEntry {discovered_doc_id} changed logical key/order while freezing timeline provenance"
            );
        }
        let agent_did = exact_row
            .agent_did
            .as_deref()
            .filter(|agent_did| !agent_did.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("CompactionEntry {discovered_doc_id} has no agent DID")
            })?;
        let fork_source = complete_edge_doc_id(
            exact_row.fork_source_doc_id.as_deref(),
            exact_row.fork_source_composite_commit_cid.as_deref(),
            exact_row.fork_source_signer_did.as_deref(),
            "CompactionEntry fork source",
        )?;
        if fork_source.is_none() && agent_did != exact.exact.signer_did {
            anyhow::bail!(
                "CompactionEntry {discovered_doc_id} signer {} does not match agent {agent_did}",
                exact.exact.signer_did
            );
        }
        compaction.row = exact_row;
        compaction.exact = Some(exact.exact.clone());
        sources.push(exact);
    }
    Ok(sources)
}

async fn load_timeline_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Option<TimelineSessionRow>> {
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ session_id: {{ _eq: "{}" }} }}
            ) {{
                _docID
                session_id
                agent_name
                behavior_id
                started
                ended
                status
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    let rows = load_rows::<TimelineSessionRow>(access, "AgentSession", &query).await?;
    match rows.as_slice() {
        [] => Ok(None),
        [session] => Ok(Some(session.clone())),
        rows => anyhow::bail!(
            "session id {session_id} matches {} AgentSession documents; refusing ambiguous projection provenance",
            rows.len()
        ),
    }
}

async fn load_timeline_conversation(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Option<TimelineConversationRow>> {
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{}" }} }}
            ) {{
                _docID
                session_id
                agent_name
                agent_did
                behavior_id
                title
                title_source
                preview_text
                status
                created_at
                updated_at
                latest_request_id
                forked_from_session_id
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    let rows = load_rows::<TimelineConversationRow>(access, "AgentConversation", &query).await?;
    match rows.as_slice() {
        [] => Ok(None),
        [conversation] => Ok(Some(conversation.clone())),
        rows => anyhow::bail!(
            "session id {session_id} matches {} AgentConversation documents; refusing ambiguous projection provenance",
            rows.len()
        ),
    }
}

fn ensure_unique_timeline_request_ids(rows: &[TimelineRequestRow]) -> Result<()> {
    let mut documents_by_request_id = std::collections::BTreeMap::<&str, &str>::new();
    for row in rows {
        let doc_id = row
            .doc_id
            .as_deref()
            .map(str::trim)
            .filter(|doc_id| !doc_id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("AgentRequest {} omitted _docID", row.request_id))?;
        if let Some(existing_doc_id) = documents_by_request_id.insert(&row.request_id, doc_id) {
            if existing_doc_id != doc_id {
                anyhow::bail!(
                    "request id {} matches multiple AgentRequest documents ({existing_doc_id}, {doc_id}); exact provenance is ambiguous",
                    row.request_id
                );
            }
        }
    }
    Ok(())
}

fn reject_rendered_request_twins(rows: &[TimelineRenderedRequestRow]) -> Result<()> {
    let mut documents_by_key = std::collections::BTreeMap::<&str, &str>::new();
    for row in rows {
        let doc_id = row
            .row
            .doc_id
            .as_deref()
            .map(str::trim)
            .filter(|doc_id| !doc_id.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("RenderedRequest {} omitted _docID", row.row.capture_key)
            })?;
        if let Some(existing_doc_id) = documents_by_key.insert(row.row.capture_key.as_str(), doc_id)
        {
            if existing_doc_id != doc_id {
                anyhow::bail!(
                    "RenderedRequest capture key {} maps to multiple documents ({existing_doc_id}, {doc_id})",
                    row.row.capture_key
                );
            }
        }
    }
    Ok(())
}

fn reject_compaction_twins(rows: &[TimelineCompactionEntryRow]) -> Result<()> {
    let mut by_key = std::collections::BTreeMap::<&str, &str>::new();
    let mut by_sequence = std::collections::BTreeMap::<(Option<&str>, Option<i64>), &str>::new();
    for row in rows {
        let doc_id = row
            .row
            .doc_id
            .as_deref()
            .map(str::trim)
            .filter(|doc_id| !doc_id.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("CompactionEntry {} omitted _docID", row.row.compaction_key)
            })?;
        if let Some(existing_doc_id) = by_key.insert(row.row.compaction_key.as_str(), doc_id) {
            if existing_doc_id != doc_id {
                anyhow::bail!(
                    "CompactionEntry key {} maps to multiple documents ({existing_doc_id}, {doc_id})",
                    row.row.compaction_key
                );
            }
        }
        let sequence_key = (row.row.session_id.as_deref(), row.row.sequence);
        if let Some(existing_doc_id) = by_sequence.insert(sequence_key, doc_id) {
            if existing_doc_id != doc_id {
                anyhow::bail!(
                    "CompactionEntry session {:?} sequence {:?} maps to multiple documents ({existing_doc_id}, {doc_id})",
                    row.row.session_id,
                    row.row.sequence
                );
            }
        }
    }
    Ok(())
}

fn reject_timeline_semantic_twins(
    messages: &[TimelineMessageRow],
    tool_calls: &[TimelineToolCallRow],
    responses: &[TimelineResponseRow],
    inference_calls: &[TimelineInferenceCallRow],
) -> Result<()> {
    let mut message_keys = std::collections::BTreeMap::new();
    for row in messages {
        let doc_id = required_source_doc_id(
            "AgentMessage",
            &format!("{}:{}", row.session_id, row.sequence),
            row.doc_id.as_deref(),
        )?;
        let key = (row.session_id.as_str(), row.sequence);
        if let Some(existing) = message_keys.insert(key, doc_id) {
            anyhow::bail!(
                "AgentMessage session {} sequence {} is ambiguous across {:?} and {:?}",
                row.session_id,
                row.sequence,
                existing,
                row.doc_id
            );
        }
    }

    let mut tool_call_keys = std::collections::BTreeMap::new();
    for row in tool_calls {
        let doc_id = required_source_doc_id(
            "AgentToolCall",
            row.tool_call_id.as_str(),
            row.doc_id.as_deref(),
        )?;
        let key = (row.session_id.as_str(), row.tool_call_id.as_str());
        if let Some(existing) = tool_call_keys.insert(key, doc_id) {
            anyhow::bail!(
                "AgentToolCall session {} call {} is ambiguous across {:?} and {:?}",
                row.session_id,
                row.tool_call_id,
                existing,
                row.doc_id
            );
        }
    }

    let mut response_keys = std::collections::BTreeMap::new();
    for row in responses {
        let doc_id = required_source_doc_id(
            "AgentResponse",
            row.request_id.as_str(),
            row.doc_id.as_deref(),
        )?;
        if let Some(existing) = response_keys.insert(row.request_id.as_str(), doc_id) {
            anyhow::bail!(
                "AgentResponse request {} is ambiguous across {:?} and {:?}",
                row.request_id,
                existing,
                row.doc_id
            );
        }
    }

    let mut inference_keys = std::collections::BTreeMap::new();
    let mut inference_slots = std::collections::BTreeMap::new();
    for row in inference_calls {
        let doc_id =
            required_source_doc_id("InferenceCall", row.call_id.as_str(), row.doc_id.as_deref())?;
        if let Some(existing) = inference_keys.insert(row.call_id.as_str(), doc_id) {
            anyhow::bail!(
                "InferenceCall {} is ambiguous across {:?} and {:?}",
                row.call_id,
                existing,
                row.doc_id
            );
        }
        let slot = (row.request_id.as_str(), row.call_seq, row.attempt);
        if let Some(existing_call_id) = inference_slots.insert(slot, row.call_id.as_str()) {
            anyhow::bail!(
                "InferenceCall request {} sequence {} attempt {} is ambiguous across calls {} and {}",
                row.request_id,
                row.call_seq,
                row.attempt,
                existing_call_id,
                row.call_id
            );
        }
    }
    Ok(())
}

fn timeline_reader_identity(node: &defra_node::EmbeddedNode) -> Result<identity::Did> {
    let node_did = node.node_identity_did().ok_or_else(|| {
        anyhow::anyhow!("exact timeline reads require a DefraDB node signing identity")
    })?;
    identity::Did::new(node_did).context("parsing timeline reader DID")
}

fn merge_timeline_request(
    rows: &mut Vec<TimelineRequestRow>,
    request: TimelineRequestRow,
) -> Result<()> {
    if let Some(existing) = rows
        .iter_mut()
        .find(|row| row.request_id == request.request_id)
    {
        if existing.doc_id != request.doc_id {
            anyhow::bail!(
                "request id {} maps to both {:?} and {:?}; use an exact document selector",
                request.request_id,
                existing.doc_id,
                request.doc_id
            );
        }
        *existing = request;
    } else {
        rows.push(request);
    }
    Ok(())
}

fn timeline_session_ids(requests: &[TimelineRequestRow]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|request| {
            request
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn timeline_request_ids(requests: &[TimelineRequestRow]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|request| {
            let request_id = request.request_id.trim();
            (!request_id.is_empty()).then_some(request_id.to_string())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn load_rows<T>(access: &ConfigAccess, collection: &str, query: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    rows_or_empty_if_collection_missing(access, collection, query)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} rows"))
}

async fn rows_or_empty_if_collection_missing(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let rows = match execute_timeline_query(access, query).await {
        Ok(response) => Ok(graphql_rows_from_response(&response, collection_name)),
        Err(error) => Err(error),
    };
    match rows {
        Ok(rows) => Ok(rows),
        Err(error)
            if {
                let message = error.to_string();
                message.contains(collection_name)
                    && (message.contains("collection not found")
                        || message.contains("Cannot query field"))
            } =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

async fn execute_timeline_query(access: &ConfigAccess, query: &str) -> Result<Value> {
    match access {
        ConfigAccess::Graphql(_) => access.execute(query).await,
        ConfigAccess::Local(node) => {
            let identity = timeline_reader_identity(node)?;
            let response = node
                .execute_request_with_retry(
                    defra_node::QueryRequest::new(query.to_string()).with_identity(Some(identity)),
                    defra_node::ExecuteRetryPolicy::default(),
                )
                .await;
            if response.has_errors() {
                anyhow::bail!("timeline GraphQL returned errors: {:?}", response.errors);
            }
            Ok(serde_json::json!({
                "data": response.data.unwrap_or(Value::Null),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(doc_id: &str, cid: &str) -> crate::SignedDocumentVersionRef {
        crate::SignedDocumentVersionRef {
            version: crate::DocumentVersionRef {
                doc_id: doc_id.to_string(),
                composite_commit_cid: cid.to_string(),
            },
            signer_did: "did:key:test".to_string(),
        }
    }

    fn exact_source(
        exact: crate::SignedDocumentVersionRef,
        collection_version_id: &str,
    ) -> ExactDocumentSource {
        ExactDocumentSource {
            exact,
            collection_version_id: collection_version_id.to_string(),
        }
    }

    fn config_fact(collection: &str, logical_id: &str, doc_id: &str) -> crate::ConfigFactRef {
        crate::ConfigFactRef {
            collection: collection.to_string(),
            logical_id: logical_id.to_string(),
            collection_version_id: format!("schema-{collection}"),
            source: signed(doc_id, &format!("cid-{doc_id}")),
        }
    }

    fn rendered_row_with_recursive_provenance() -> gents_protocol::row::RenderedRequestRow {
        let request = crate::RequestExecutionProvenance::new(
            crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new("request-doc", "cid-request-source"),
                "did:key:requester",
            ),
            crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new("request-doc", "cid-request-claim"),
                "did:key:agent",
            ),
        );
        let inference = crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new("inference-doc", "cid-inference"),
            "did:key:agent",
        );
        let config = crate::ResolvedBehaviorConfigProvenance {
            principal: config_fact("AgentPrincipal", "did:key:agent", "principal-doc"),
            behavior: config_fact("AgentBehavior", "behavior", "behavior-doc"),
            inference_backend: config_fact("InferenceBackend", "backend", "backend-doc"),
            inference_profile: config_fact("InferenceProfile", "profile", "profile-doc"),
            tool_selection: Some(config_fact("ToolSelection", "selection", "selection-doc")),
            datastore_tool_surfaces: Vec::new(),
            skills: vec![config_fact("Skill", "skill", "skill-doc")],
            resolution_algorithm_version: 1,
        };
        let transcript = vec![crate::MessageFactRef {
            sequence: 1,
            doc_id: "message-doc".to_string(),
            composite_commit_cid: "cid-message".to_string(),
            collection_version_id: "schema-AgentMessage".to_string(),
            signer_did: "did:key:agent".to_string(),
        }];
        let manifest = crate::rendered_request::ProvenanceManifest::captured_only_with_admission_and_scoped_config_provenance(
            "inference.1".to_string(),
            Some("https://provider.example".to_string()),
            Some(request),
            transcript,
            crate::rendered_request::InferenceCallProvenanceScope::AdmittedProviderCall,
            Some(inference),
            crate::rendered_request::ConfigProvenanceScope::ReconciledDocumentRuntime,
            Some(config),
            crate::rendered_request::AssemblyTrace::from_effective_messages(
                crate::rendered_request::AssemblyBuildPath::Budgeted,
                Vec::new(),
            ),
        );
        gents_protocol::row::RenderedRequestRow {
            doc_id: Some("rendered-doc".to_string()),
            capture_key: "capture".to_string(),
            request_doc_id: Some("request-doc".to_string()),
            request_source_commit_cid: Some("cid-request-source".to_string()),
            request_source_signer_did: Some("did:key:requester".to_string()),
            request_claim_commit_cid: Some("cid-request-claim".to_string()),
            request_claim_signer_did: Some("did:key:agent".to_string()),
            inference_call_doc_id: Some("inference-doc".to_string()),
            inference_call_composite_commit_cid: Some("cid-inference".to_string()),
            inference_call_signer_did: Some("did:key:agent".to_string()),
            request_id: Some("request".to_string()),
            session_id: Some("session".to_string()),
            agent_did: Some("did:key:agent".to_string()),
            requester_did: Some("did:key:requester".to_string()),
            behavior_id: Some("behavior".to_string()),
            capture_scope: Some("inference.1".to_string()),
            turn_index: Some(1),
            attempt: Some(1),
            capture_version: Some(1),
            model_name: Some("model".to_string()),
            source: Some("openai_chat_completions".to_string()),
            request_json: Some("{}".to_string()),
            prompt_hash: Some("prompt".to_string()),
            tools_hash: Some("tools".to_string()),
            provenance_json: Some(serde_json::to_string(&manifest).expect("manifest JSON")),
            created_at: Some("2026-08-08T00:00:00Z".to_string()),
        }
    }

    #[test]
    fn rendered_provenance_parser_requires_canonical_recursive_exact_refs() {
        let row = rendered_row_with_recursive_provenance();
        let manifest = validated_rendered_provenance_manifest(&row).expect("valid manifest");
        assert_eq!(manifest.transcript_snapshot.len(), 1);
        assert_eq!(manifest.config_provenance.expect("config").skills.len(), 1);

        let mut missing = row.clone();
        missing.provenance_json = None;
        assert!(validated_rendered_provenance_manifest(&missing).is_err());

        let mut mismatched = row.clone();
        mismatched.request_claim_signer_did = Some("did:key:forged".to_string());
        assert!(validated_rendered_provenance_manifest(&mismatched).is_err());

        let mut malformed = row;
        let mut value: Value =
            serde_json::from_str(malformed.provenance_json.as_deref().expect("provenance"))
                .expect("manifest value");
        value["config_provenance"]["principal"]["collection_version_id"] =
            Value::String(String::new());
        malformed.provenance_json = Some(serde_json::to_string(&value).expect("manifest JSON"));
        assert!(validated_rendered_provenance_manifest(&malformed).is_err());

        let mut missing_nested = rendered_row_with_recursive_provenance();
        let mut value: Value = serde_json::from_str(
            missing_nested
                .provenance_json
                .as_deref()
                .expect("provenance"),
        )
        .expect("manifest value");
        value["config_provenance"] = Value::Null;
        missing_nested.provenance_json =
            Some(serde_json::to_string(&value).expect("manifest JSON"));
        assert!(validated_rendered_provenance_manifest(&missing_nested).is_err());
    }

    #[test]
    fn rendered_nested_snapshot_rejects_schema_signer_and_logical_rebinding() {
        let expected = signed("config-doc", "config-cid");
        let snapshot = VerifiedExactDocumentSnapshot {
            source: expected.clone(),
            collection_version_id: "schema-v1".to_string(),
            document: serde_json::json!({ "behavior_id": "behavior" }),
        };
        validate_rendered_nested_snapshot(
            "AgentBehavior",
            &snapshot,
            &expected,
            "schema-v1",
            Some(("behavior_id", "behavior")),
        )
        .expect("exact nested source");
        assert!(validate_rendered_nested_snapshot(
            "AgentBehavior",
            &snapshot,
            &expected,
            "schema-v2",
            Some(("behavior_id", "behavior")),
        )
        .is_err());
        let mut rebound_signer = expected.clone();
        rebound_signer.signer_did = "did:key:forged".to_string();
        assert!(validate_rendered_nested_snapshot(
            "AgentBehavior",
            &snapshot,
            &rebound_signer,
            "schema-v1",
            Some(("behavior_id", "behavior")),
        )
        .is_err());
        assert!(validate_rendered_nested_snapshot(
            "AgentBehavior",
            &snapshot,
            &expected,
            "schema-v1",
            Some(("behavior_id", "forged")),
        )
        .is_err());
    }

    #[test]
    fn logical_request_twins_are_rejected_by_physical_document() {
        let rows = [
            TimelineRequestRow {
                doc_id: Some("doc-a".to_string()),
                request_id: "same".to_string(),
                ..TimelineRequestRow::default()
            },
            TimelineRequestRow {
                doc_id: Some("doc-b".to_string()),
                request_id: "same".to_string(),
                ..TimelineRequestRow::default()
            },
        ];
        let error = ensure_unique_timeline_request_ids(&rows).expect_err("logical twins");
        assert!(error
            .to_string()
            .contains("multiple AgentRequest documents"));
    }

    #[test]
    fn timeline_sources_without_physical_document_ids_fail_closed() {
        for doc_id in [None, Some("   ".to_string())] {
            let requests = [TimelineRequestRow {
                doc_id: doc_id.clone(),
                request_id: "request".to_string(),
                ..TimelineRequestRow::default()
            }];
            assert!(ensure_unique_timeline_request_ids(&requests).is_err());

            let rendered = [TimelineRenderedRequestRow {
                row: gents_protocol::row::RenderedRequestRow {
                    doc_id: doc_id.clone(),
                    capture_key: "capture".to_string(),
                    request_doc_id: None,
                    request_source_commit_cid: None,
                    request_source_signer_did: None,
                    request_claim_commit_cid: None,
                    request_claim_signer_did: None,
                    inference_call_doc_id: None,
                    inference_call_composite_commit_cid: None,
                    inference_call_signer_did: None,
                    request_id: None,
                    session_id: None,
                    agent_did: None,
                    requester_did: None,
                    behavior_id: None,
                    capture_scope: None,
                    turn_index: None,
                    attempt: None,
                    capture_version: None,
                    model_name: None,
                    source: None,
                    request_json: None,
                    prompt_hash: None,
                    tools_hash: None,
                    provenance_json: None,
                    created_at: None,
                },
                exact: None,
                request_json_field_cid: None,
            }];
            assert!(reject_rendered_request_twins(&rendered).is_err());

            let compactions = [TimelineCompactionEntryRow {
                row: gents_protocol::row::CompactionEntryRow {
                    doc_id: doc_id.clone(),
                    compaction_key: "compaction".to_string(),
                    session_id: Some("session".to_string()),
                    agent_did: None,
                    requester_did: None,
                    sequence: Some(1),
                    summary: None,
                    files_read: None,
                    files_modified: None,
                    messages_compacted: None,
                    original_tokens: None,
                    compacted_tokens: None,
                    source_manifest_version: None,
                    source_manifest_json: None,
                    created_at: None,
                    fork_source_doc_id: None,
                    fork_source_composite_commit_cid: None,
                    fork_source_signer_did: None,
                },
                exact: None,
            }];
            assert!(reject_compaction_twins(&compactions).is_err());

            let messages = [TimelineMessageRow {
                doc_id: doc_id.clone(),
                session_id: "session".to_string(),
                sequence: 1,
                ..TimelineMessageRow::default()
            }];
            assert!(reject_timeline_semantic_twins(&messages, &[], &[], &[]).is_err());

            let tool_calls = [TimelineToolCallRow {
                doc_id: doc_id.clone(),
                session_id: "session".to_string(),
                tool_call_id: "tool-call".to_string(),
                ..TimelineToolCallRow::default()
            }];
            assert!(reject_timeline_semantic_twins(&[], &tool_calls, &[], &[]).is_err());

            let responses = [TimelineResponseRow {
                doc_id: doc_id.clone(),
                request_id: "request".to_string(),
                ..TimelineResponseRow::default()
            }];
            assert!(reject_timeline_semantic_twins(&[], &[], &responses, &[]).is_err());

            let inference_calls = [TimelineInferenceCallRow {
                doc_id,
                call_id: "inference-call".to_string(),
                ..TimelineInferenceCallRow::default()
            }];
            assert!(reject_timeline_semantic_twins(&[], &[], &[], &inference_calls).is_err());
        }
    }

    #[test]
    fn exact_manifest_orders_root_then_all_exact_sources() {
        let root = signed("request-root", "cid-root");
        let child = signed("request-child", "cid-child");
        let message = signed("message-1", "cid-message");
        let manifest = freeze_exact_timeline_source_manifest(
            exact_source(root.clone(), "schema-request-v1"),
            vec![
                ExactTimelineSource {
                    class: TimelineSourceClass::Message,
                    collection: "AgentMessage",
                    source: exact_source(message.clone(), "schema-message-v1"),
                },
                // The same exact fact may be reached from the open timeline
                // scan and a nested RenderedRequest provenance edge. It is one
                // manifest source, not two provenance claims.
                ExactTimelineSource {
                    class: TimelineSourceClass::Message,
                    collection: "AgentMessage",
                    source: exact_source(message.clone(), "schema-message-v1"),
                },
                ExactTimelineSource {
                    class: TimelineSourceClass::Request,
                    collection: "AgentRequest",
                    source: exact_source(child.clone(), "schema-request-v1"),
                },
                ExactTimelineSource {
                    class: TimelineSourceClass::Request,
                    collection: "AgentRequest",
                    source: exact_source(root.clone(), "schema-request-v1"),
                },
            ],
            &[],
            &[],
        )
        .expect("exact manifest");

        assert_eq!(manifest.root, root);
        assert_eq!(manifest.sources.len(), 3);
        assert_eq!(manifest.sources[0].slot, TimelineSourceSlot::root());
        assert_eq!(manifest.sources[0].exact.version.doc_id, "request-root");
        assert_eq!(
            manifest.sources[0].collection_version_id,
            "schema-request-v1"
        );
        assert_eq!(
            manifest.sources[1].slot.source_class,
            TimelineSourceClass::Request
        );
        assert_eq!(manifest.sources[1].slot.ordinal, 1);
        assert_eq!(manifest.sources[1].exact, child);
        assert_eq!(
            manifest.sources[2].slot.source_class,
            TimelineSourceClass::Message
        );
        assert_eq!(manifest.sources[2].slot.ordinal, 0);
        assert_eq!(manifest.sources[2].exact, message);
    }

    #[test]
    fn production_open_scans_emit_a_partial_exact_manifest_with_explicit_gaps() {
        let root = signed("request-root", "cid-root");
        let rows = RunTimelineRows {
            request: TimelineRequestRow {
                doc_id: Some("request-root".to_string()),
                request_id: "logical-root".to_string(),
                session_id: Some("session-root".to_string()),
                ..TimelineRequestRow::default()
            },
            requests: vec![TimelineRequestRow {
                doc_id: Some("request-root".to_string()),
                request_id: "logical-root".to_string(),
                session_id: Some("session-root".to_string()),
                ..TimelineRequestRow::default()
            }],
            ..RunTimelineRows::default()
        };
        let gaps = open_timeline_coverage_gaps(&rows, &root);
        assert!(!gaps.is_empty());
        assert!(gaps.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(gaps.iter().any(|gap| {
            gap.kind == TimelineCoverageGapKind::NonAtomicObservation
                && gap.source_class == TimelineSourceClass::Request
        }));
        assert!(gaps.iter().any(|gap| {
            gap.kind == TimelineCoverageGapKind::OpenLogicalExtent
                && gap.source_class == TimelineSourceClass::InferenceCall
                && gap.scope_id == "logical-root"
        }));
        assert!(gaps.iter().any(|gap| {
            gap.kind == TimelineCoverageGapKind::OpenSessionExtent
                && gap.source_class == TimelineSourceClass::Message
                && gap.scope_id == "session-root"
        }));

        let manifest = freeze_exact_timeline_source_manifest(
            exact_source(root.clone(), "schema-request-v1"),
            vec![ExactTimelineSource {
                class: TimelineSourceClass::Request,
                collection: "AgentRequest",
                source: exact_source(root, "schema-request-v1"),
            }],
            &gaps,
            &[],
        )
        .expect("partial exact manifest");
        assert_eq!(
            manifest.status,
            crate::run_timeline_manifest::TimelineManifestStatus::PartialExact
        );
        assert_eq!(manifest.coverage_gaps, gaps);
        manifest.validate().expect("canonical partial manifest");
    }

    #[test]
    fn production_manifest_freezer_enforces_declared_exact_edge_closure() {
        let root = signed("request-root", "cid-root");
        let missing = signed("message-missing", "cid-message-missing");
        let error = freeze_exact_timeline_source_manifest(
            exact_source(root.clone(), "schema-request-v1"),
            vec![ExactTimelineSource {
                class: TimelineSourceClass::Request,
                collection: "AgentRequest",
                source: exact_source(root, "schema-request-v1"),
            }],
            &[],
            &[TimelineDeclaredExactEdge {
                collection: "AgentMessage".to_string(),
                collection_version_id: "schema-message-v1".to_string(),
                exact: missing,
            }],
        )
        .expect_err("production freezer must reject an omitted declared edge");

        assert!(
            matches!(
                error.downcast_ref::<crate::run_timeline_manifest::TimelineManifestError>(),
                Some(
                    crate::run_timeline_manifest::TimelineManifestError::MissingDeclaredExactEdge(
                        _
                    )
                )
            ),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn rendered_and_compaction_logical_twins_fail_closed() {
        let rendered = |doc_id: &str| TimelineRenderedRequestRow {
            row: gents_protocol::row::RenderedRequestRow {
                doc_id: Some(doc_id.to_string()),
                capture_key: "capture".to_string(),
                request_doc_id: None,
                request_source_commit_cid: None,
                request_source_signer_did: None,
                request_claim_commit_cid: None,
                request_claim_signer_did: None,
                inference_call_doc_id: None,
                inference_call_composite_commit_cid: None,
                inference_call_signer_did: None,
                request_id: None,
                session_id: None,
                agent_did: None,
                requester_did: None,
                behavior_id: None,
                capture_scope: None,
                turn_index: None,
                attempt: None,
                capture_version: None,
                model_name: None,
                source: None,
                request_json: None,
                prompt_hash: None,
                tools_hash: None,
                provenance_json: None,
                created_at: None,
            },
            exact: None,
            request_json_field_cid: None,
        };
        assert!(reject_rendered_request_twins(&[rendered("a"), rendered("b")]).is_err());

        let compaction = |doc_id: &str| TimelineCompactionEntryRow {
            row: gents_protocol::row::CompactionEntryRow {
                doc_id: Some(doc_id.to_string()),
                compaction_key: "compact".to_string(),
                session_id: Some("session".to_string()),
                agent_did: None,
                requester_did: None,
                sequence: Some(1),
                summary: None,
                files_read: None,
                files_modified: None,
                messages_compacted: None,
                original_tokens: None,
                compacted_tokens: None,
                source_manifest_version: None,
                source_manifest_json: None,
                created_at: None,
                fork_source_doc_id: None,
                fork_source_composite_commit_cid: None,
                fork_source_signer_did: None,
            },
            exact: None,
        };
        assert!(reject_compaction_twins(&[compaction("a"), compaction("b")]).is_err());
    }

    #[test]
    fn timeline_event_semantic_twins_fail_closed() {
        let messages = [
            TimelineMessageRow {
                doc_id: Some("message-a".to_string()),
                session_id: "session".to_string(),
                sequence: 1,
                ..TimelineMessageRow::default()
            },
            TimelineMessageRow {
                doc_id: Some("message-b".to_string()),
                session_id: "session".to_string(),
                sequence: 1,
                ..TimelineMessageRow::default()
            },
        ];
        assert!(reject_timeline_semantic_twins(&messages, &[], &[], &[]).is_err());

        let calls = [
            TimelineInferenceCallRow {
                doc_id: Some("call-a".to_string()),
                call_id: "call".to_string(),
                ..TimelineInferenceCallRow::default()
            },
            TimelineInferenceCallRow {
                doc_id: Some("call-b".to_string()),
                call_id: "call".to_string(),
                ..TimelineInferenceCallRow::default()
            },
        ];
        assert!(reject_timeline_semantic_twins(&[], &[], &[], &calls).is_err());
    }

    #[test]
    fn inference_calls_with_distinct_ids_cannot_share_a_request_slot() {
        let calls = [
            TimelineInferenceCallRow {
                doc_id: Some("call-doc-a".to_string()),
                call_id: "call-a".to_string(),
                request_id: "request".to_string(),
                call_seq: 3,
                attempt: 2,
                ..TimelineInferenceCallRow::default()
            },
            TimelineInferenceCallRow {
                doc_id: Some("call-doc-b".to_string()),
                call_id: "call-b".to_string(),
                request_id: "request".to_string(),
                call_seq: 3,
                attempt: 2,
                ..TimelineInferenceCallRow::default()
            },
        ];

        let error = reject_timeline_semantic_twins(&[], &[], &[], &calls)
            .expect_err("one request slot must map to one inference call");
        assert!(error.to_string().contains("ambiguous across calls"));
    }

    fn terminal_omission_call() -> TimelineToolCallRow {
        TimelineToolCallRow {
            tool_call_id: "tool-call".to_string(),
            lifecycle_state: Some("failed".to_string()),
            omission_doc_id: Some("omission-doc".to_string()),
            omission_composite_commit_cid: Some("omission-cid".to_string()),
            omission_signer_did: Some("did:key:test".to_string()),
            ..TimelineToolCallRow::default()
        }
    }

    #[test]
    fn terminal_tool_call_requires_exactly_one_outcome_edge() {
        let missing = TimelineToolCallRow {
            tool_call_id: "missing".to_string(),
            lifecycle_state: Some("cancelled".to_string()),
            ..TimelineToolCallRow::default()
        };
        assert!(validate_terminal_outcome_edge_shape(&missing).is_err());

        let mut both = terminal_omission_call();
        both.result_doc_id = Some("result-doc".to_string());
        both.result_composite_commit_cid = Some("result-cid".to_string());
        both.result_signer_did = Some("did:key:test".to_string());
        assert!(validate_terminal_outcome_edge_shape(&both).is_err());

        assert!(validate_terminal_outcome_edge_shape(&terminal_omission_call()).is_ok());
    }

    #[test]
    fn unbound_outcome_proposals_do_not_replace_the_exact_terminal_edge() {
        let call = terminal_omission_call();
        assert!(validate_terminal_outcome_edge_shape(&call).is_ok());
        assert_eq!(call.omission_doc_id.as_deref(), Some("omission-doc"));
        assert!(call.result_doc_id.is_none());
    }

    #[test]
    fn exact_outcome_signature_rejects_wrong_cid_or_signer() {
        let exact = signed("omission-doc", "omission-cid");
        assert!(validate_bound_outcome_signature(
            Some("wrong-cid"),
            Some("did:key:test"),
            &exact,
            "did:key:test",
            "omission",
        )
        .is_err());
        assert!(validate_bound_outcome_signature(
            Some("omission-cid"),
            Some("did:key:other"),
            &exact,
            "did:key:test",
            "omission",
        )
        .is_err());
        assert!(validate_bound_outcome_signature(
            Some("omission-cid"),
            Some("did:key:test"),
            &exact,
            "did:key:other-parent",
            "omission",
        )
        .is_err());
    }

    #[test]
    fn omission_reason_must_match_source_and_terminal_phases() {
        assert!(omission_reason_allows("executionLost", "running", "failed"));
        assert!(omission_reason_allows(
            "approvalDenied",
            "awaitingApproval",
            "failed"
        ));
        assert!(!omission_reason_allows(
            "approvalDenied",
            "running",
            "failed"
        ));
        assert!(!omission_reason_allows("unknown", "running", "failed"));
    }

    #[test]
    fn exact_tool_call_edges_must_match_attached_fact_identities() {
        let exact = signed("tool-call-doc", "tool-call-current-cid");
        let exact_row = TimelineToolCallRow {
            doc_id: Some("tool-call-doc".to_string()),
            result_doc_id: Some("result-doc".to_string()),
            result_composite_commit_cid: Some("result-cid".to_string()),
            result_signer_did: Some("did:key:result".to_string()),
            approval_doc_id: Some("approval-doc".to_string()),
            approval_composite_commit_cid: Some("approval-cid".to_string()),
            approval_signer_did: Some("did:key:approver".to_string()),
            ..TimelineToolCallRow::default()
        };

        let matching_result = TimelineToolResultFact {
            doc_id: "result-doc".to_string(),
            composite_commit_cid: "result-cid".to_string(),
            signer_did: "did:key:result".to_string(),
            tool_call_doc_id: "tool-call-doc".to_string(),
            tool_call_composite_commit_cid: "tool-call-historical-cid".to_string(),
            tool_call_signer_did: "did:key:test".to_string(),
            output_text: "result".to_string(),
        };
        let matching_approval = TimelineToolApprovalFact {
            doc_id: "approval-doc".to_string(),
            composite_commit_cid: "approval-cid".to_string(),
            signer_did: "did:key:approver".to_string(),
            tool_call_doc_id: "tool-call-doc".to_string(),
            tool_call_composite_commit_cid: "tool-call-historical-cid".to_string(),
            tool_call_signer_did: "did:key:test".to_string(),
            decision: "approved".to_string(),
            reason: None,
        };

        let result_mismatch = TimelineToolCallRow {
            result_fact: Some(TimelineToolResultFact {
                composite_commit_cid: "different-result-cid".to_string(),
                ..matching_result.clone()
            }),
            approval_fact: Some(matching_approval.clone()),
            ..TimelineToolCallRow::default()
        };
        let error = validate_exact_tool_fact_edges(&exact_row, &exact, &result_mismatch)
            .expect_err("a stale result edge must fail closed");
        assert!(error.to_string().contains("exact result edge changed"));

        let approval_mismatch = TimelineToolCallRow {
            result_fact: Some(matching_result),
            approval_fact: Some(TimelineToolApprovalFact {
                signer_did: "did:key:different-approver".to_string(),
                ..matching_approval
            }),
            ..TimelineToolCallRow::default()
        };
        let error = validate_exact_tool_fact_edges(&exact_row, &exact, &approval_mismatch)
            .expect_err("a stale approval edge must fail closed");
        assert!(error.to_string().contains("exact approval edge changed"));
    }
}
