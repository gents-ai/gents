use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use gents::document_config::{
    Agent, AgentContext, AgentTargetDocument, ChainKeyBindingDocument, CompactionConfig,
    DatastoreToolSurfaceDocument, EventSource, InferenceBackend, InferenceBackendObservation,
    InferenceExecution, InferenceProfile, InferenceSampling, Node, Schedule, ScheduleObservation,
    SkillDocument, Task, ToolServiceRegistry, Tools, Trigger, TriggerObservation,
};
use gents_protocol::graphql::escape_graphql_string;
use gents_protocol::output::reconstruction::DependencyDenial;
use gents_protocol::row::{
    AgentRequestRow, AgentToolCallRow, CompactionEntryRow, GoalRow, MailboxItemRow,
    NodeReadinessRow, NodeRuntimeRow,
};
use gents_protocol::schemas::{
    AGENT_MESSAGE_NAME, AGENT_NAME, AGENT_OUTPUT_SEGMENT_NAME, AGENT_REQUEST_NAME,
    AGENT_SESSION_NAME, AGENT_TARGET_NAME, AGENT_TOOL_CALL_NAME, COMPACTION_ENTRY_NAME, GOAL_NAME,
    INFERENCE_BACKEND_NAME, INFERENCE_PROFILE_NAME, MAILBOX_ITEM_NAME, NODE_NAME,
    NODE_READINESS_NAME, NODE_RUNTIME_NAME, SCHEDULE_NAME, SKILL_NAME, TASK_NAME, TOOLS_NAME,
    TOOL_SERVICE_REGISTRY_NAME, TRIGGER_NAME,
};
use gents_protocol::session::AgentSession;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

use super::peer_directory::PeerRecord;
use super::store::{ClientStore, ClientStoreRows};

mod agent_scope;
mod document_patches;
mod request_closure;
mod session_tip;
mod session_transcript;
pub use session_tip::{
    load_request_prompt_ownership, load_request_prompt_ownership_on, load_session_live_store,
    load_session_live_store_on, load_session_tip_store, load_session_tip_store_on,
    RequestPromptFact, RequestPromptOwnership,
};
mod snapshot_loaders;

pub use agent_scope::load_node_scoped_snapshot;
pub use document_patches::fetch_doc_patch;
pub(crate) use document_patches::{
    is_transcript_content_collection, supports_doc_patch_collection,
};
pub(crate) use request_closure::{close_request_rows, close_request_rows_on};
pub use session_transcript::{
    load_session_compaction_entries, load_session_compaction_entries_on,
    load_session_context_store, load_session_context_store_on, load_session_diagnostics_store,
    load_session_transcript_page, load_session_transcript_page_on, load_session_transcript_window,
    load_session_transcript_window_on, session_transcript_requester_scope,
    session_unreadable_reason, session_unsendable_reason, TranscriptWindow,
};
pub(crate) use snapshot_loaders::*;

pub const DEFAULT_SESSION_TRANSCRIPT_PAGE_SIZE: usize = 40;
pub const MAX_SESSION_TRANSCRIPT_PAGE_SIZE: usize = 80;
pub(super) const SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET: usize = 320;

#[derive(Debug)]
pub struct SessionTranscriptQueryPage {
    pub store: ClientStore,
    pub canonical_dependencies: CanonicalTranscriptDependencies,
    pub query_count: u64,
    pub queried_rows: usize,
    pub message_query_limit: usize,
    pub tool_call_query_limit: usize,
    pub source_exhausted: bool,
    pub has_newer: bool,
    pub covered: TranscriptSpan,
    /// The requests the page's rows belong to, as far as they are readable.
    /// Kept beside the store so a reader merging pages into its own request
    /// state does not take them for the session's request history.
    pub requests: Vec<AgentRequestRow>,
    /// Delivery messages outside the page that carry results for its tool
    /// calls: sources for those results, never rows of their own.
    pub result_messages: Vec<gents::session::canonical_rows::TranscriptMessageRow>,
}

/// The message numbers a transcript read proves, inclusive. Every message in
/// the span is in the read, so a number inside it without a row is empty, not
/// unread. `through: None` reaches the newest message the read could see.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptSpan {
    pub from: Option<i64>,
    pub through: Option<i64>,
}

#[derive(Debug, Default)]
pub struct CanonicalTranscriptDependencies {
    pub origin_headers: Vec<gents::session::canonical_rows::TranscriptMessageRow>,
    pub output_segments: Vec<gents::session::canonical_rows::OutputSegmentRow>,
    pub denied_header_doc_ids: Vec<String>,
    pub denied_segment_doc_ids: Vec<String>,
    pub dependency_denials: Vec<DependencyDenial>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TranscriptCursorRow {
    pub(super) sequence: Option<i64>,
}

pub(super) const NODE_FIELDS: &str =
    "node_did display_name default_agent_id enabled created_at created_by max_request_hop tags";
pub(super) const AGENT_FIELDS: &str = "agent_id node_did display_name description context_id inference_profile_id enabled tags created_at";
pub(super) const NODE_RUNTIME_FIELDS: &str = "node_did reconcile_phase agent_executor_capacity agent_executor_queue_depth agent_executor_status_json last_reconcile_result last_reconcile_error last_reconcile_completed_at updated_at";
pub(super) const NODE_READINESS_FIELDS: &str = "node_did snapshot_json updated_at";
pub(super) const AGENT_REQUEST_FIELDS: &str = "_docID request_id purpose node_did requester_did agent_id session_id retry_parent_request retry_parent_request_doc_id retry_root_request superseded_by_request superseded_by_request_doc_id content max_total_tokens input lifecycle_state backend_id execution_origin execution_generation execution_lease_secs execution_lease_expires_at caused_by_trigger_id caused_by_trigger_kind caused_by_correlation caused_by_trigger_context caused_by_source_doc_id caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id failure_reason terminalized_at terminal_output stopped_before_running terminal_redrive_attempts created_at claimed_at deadline retry_count max_retries interrupt_requested_at valid_until request_hop workspace_id workspace_authority workspace_owner_node_did workspace_seal_hash";
pub(super) const AGENT_SESSION_FIELDS: &str = "session_id node_did requester_did agent_id created_at closed_at title tags provenance observation";
pub(super) const GOAL_FIELDS: &str = "goal_id session_id node_did creation_key objective status token_budget tokens_used active_time_seconds active_started_at consecutive_blocked_audits last_blocked_request_id last_blocked_reason last_continued_from_request_id continuation_sequence wrapup_requested wrapup_completed infrastructure_retry_count last_failure completion_evidence created_at updated_at";
pub(super) const AGENT_TOOL_CALL_FIELDS: &str = "_docID tool_call_key node_did session_id request_id request_doc_id requester_did message_sequence tool_name tool_call_id status lifecycle_state await_mode deadline_at cancel_cause started_at completed_at selected_service_id selected_tool_name tool_failure_class denial_reason denied_argv denied_command denied_argument denied_subcommand denied_prefix policy_mode policy_network latency_ms job_pin job_progress";
pub(super) const COMPACTION_ENTRY_FIELDS: &str = "compaction_key session_id requester_did sequence summary files_read files_modified messages_compacted compacted_through_sequence original_tokens compacted_tokens created_at";
pub(super) const TASK_FIELDS: &str = "task_id node_did display_name description agent_id prompt_template emit_outcome goal_objective_template goal_token_budget hooks enabled output_schema_ref created_at updated_at tags";
pub(super) const SKILL_FIELDS: &str = "skill_id node_did name description instructions source_directory tool_refs display_name interface_json enabled created_at tags";
pub(super) const SCHEDULE_FIELDS: &str =
    "schedule_id node_did display_name cadence created_at updated_at tags";
pub(super) const SCHEDULE_OBSERVATION_FIELDS: &str = "trigger_id next_run_at";
pub(super) const TRIGGER_FIELDS: &str = "node_did trigger_id display_name description task_id source session_id_template enabled concurrency created_at updated_at tags";
pub(super) const TRIGGER_OBSERVATION_FIELDS: &str =
    "trigger_id last_attempt_at last_fired_source_doc_id last_status last_error fire_count";
pub(super) const TOOLS_FIELDS: &str = "tools_id node_did display_name host remote agents built_ins datastore integrations self_config tags";
pub(super) const AGENT_CONTEXT_FIELDS: &str = "context_id node_did display_name description system_prompt tools_id compaction_id skill_ids tags";
pub(super) const COMPACTION_CONFIG_FIELDS: &str = "compaction_id node_did display_name strategy threshold keep_recent_tokens tool_result_max_chars summary_max_output_tokens summary_file_list_max inference_profile_id tags";
pub(super) const INFERENCE_BACKEND_FIELDS: &str = "backend_id node_did name provider_kind openai_wire_api endpoint auth connect_timeout_secs discovery_timeout_secs max_concurrent max_queue_depth enabled tags";
pub(super) const INFERENCE_BACKEND_OBSERVATION_FIELDS: &str =
    "backend_id catalogs last_probe probe_status";
pub(super) const INFERENCE_PROFILE_FIELDS: &str = "profile_id node_did display_name description backend_id model_name reasoning_effort service_tier context_window max_output_tokens sampling_id execution_id tags";
pub(super) const INFERENCE_SAMPLING_FIELDS: &str = "sampling_id node_did display_name temperature top_p top_k seed min_p frequency_penalty presence_penalty repetition_penalty tags";
pub(super) const INFERENCE_EXECUTION_FIELDS: &str = "execution_id node_did display_name max_turns max_total_tokens stream_batch_ms stream_liveness_timeout_secs provider_idle_timeout_secs deadline_duration_secs retry_policy_id tags";
pub(super) const TOOL_SERVICE_REGISTRY_FIELDS: &str = "service_id node_did display_name description hostname tailscale_ip lan_ip mcp_port mcp_path send_node_did enabled tags";
pub(super) const EVENT_SOURCE_FIELDS: &str = "event_source_id node_did display_name source_collection event_kind filter correlation_field group workspace_authority created_at updated_at tags";
pub(super) const AGENT_TARGET_FIELDS: &str =
    "target_id node_did target_node_did agent_id name description tags";
pub(super) const DATASTORE_TOOL_SURFACE_FIELDS: &str =
    "surface_id node_did display_name enabled entries created_at tags";
pub(super) const CHAIN_KEY_BINDING_FIELDS: &str =
    "binding_id node_did address key_backend attestation created_at revoked_at tags";
pub(super) const MAILBOX_ITEM_FIELDS: &str = "_docID item_key requester_did node_did status kind action title summary payload source_kind source_id session_id request_id graph_run_id cause_doc_id target_node_did target_agent_id expected_collection parent_item_id deadline_at created_at updated_at resolved_at resolved_doc_id";

pub async fn load_chat_patch(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    request_id: Option<&str>,
    fallback_request_id: Option<&str>,
    page_request_doc_ids: &[String],
) -> Result<ClientStore> {
    access
        .transact_readonly("desktop.selected_request_refresh", |txn| {
            let node_did = node_did.to_owned();
            let session_id = session_id.to_owned();
            let request_id = request_id.map(str::to_owned);
            let fallback_request_id = fallback_request_id.map(str::to_owned);
            let page_request_doc_ids = page_request_doc_ids.to_vec();
            Box::pin(async move {
                let mut execute = |query: String, operation: &'static str| {
                    Box::pin(async move {
                        let response = txn
                            .execute(&query)
                            .await
                            .with_context(|| operation.to_string())?;
                        into_access_graphql_data(response, operation)
                    }) as QueryFuture<'_>
                };
                load_chat_patch_with(
                    &node_did,
                    &session_id,
                    request_id.as_deref(),
                    fallback_request_id.as_deref(),
                    &page_request_doc_ids,
                    &mut execute,
                )
                .await
            })
        })
        .await
}

type QueryFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

async fn load_chat_patch_with<'a>(
    node_did: &str,
    session_id: &str,
    request_id: Option<&str>,
    fallback_request_id: Option<&str>,
    page_request_doc_ids: &[String],
    execute: &mut (impl FnMut(String, &'static str) -> QueryFuture<'a> + 'a),
) -> Result<ClientStore> {
    let node_did = node_did.trim();
    let session_id = session_id.trim();
    let request_id = request_id.map(str::trim).filter(|id| !id.is_empty());
    let fallback_request_id = fallback_request_id
        .map(str::trim)
        .filter(|id| !id.is_empty());
    if node_did.is_empty() || session_id.is_empty() {
        return Ok(ClientStore::default());
    }

    let session_query = selected_session_query(node_did, session_id);
    let session_data = execute(session_query, "selected session owner").await?;
    let session = selected_session_from_data(&session_data, node_did, session_id)?;
    let requester_did = session.requester_did.clone();
    let lookup = selected_request_lookup(
        node_did,
        session_id,
        requester_did.as_deref(),
        session.observation.as_ref(),
        request_id,
        fallback_request_id,
        page_request_doc_ids,
    )?;
    let selected = if let Some(mut lookup) = lookup {
        let query = std::mem::take(&mut lookup.query);
        let data = execute(query, "selected request refresh").await?;
        lookup.decode(&data, node_did, session_id, requester_did.as_deref())?
    } else {
        Vec::new()
    };
    let scopes = request_seed_scopes(&selected);
    let seeds = if scopes.is_empty() {
        selected
    } else {
        let query = session_request_seed_query(&scopes, page_request_doc_ids);
        let data = execute(query, "session request seeds").await?;
        request_seeds_from_data(&data, selected, &scopes)?
    };
    let requests = request_closure::close_request_rows_with(seeds, |query| {
        execute(query, "request dependency closure")
    })
    .await?;
    let scopes = request_context_scopes(&requests);
    if scopes.is_empty() {
        return Ok(ClientStore::from_rows(ClientStoreRows {
            requests,
            sessions: vec![session],
            ..ClientStoreRows::default()
        }));
    }
    let query = request_context_query(&scopes);
    let data = execute(query, "request session context").await?;
    request_patch_from_context_data(&data, requests, vec![session], requester_did, &scopes)
}

pub(crate) async fn load_request_documents_on(
    access: &gents::config_client::ConfigAccess,
    doc_ids: &[String],
) -> Result<Vec<AgentRequestRow>> {
    let (rows, expected) = session_transcript::read_request_documents(
        session_transcript::TranscriptAccess::Config(access),
        doc_ids,
        "pending edit request patch",
    )
    .await?;
    anyhow::ensure!(
        rows.len() == expected,
        "pending edit request patch is incomplete"
    );
    Ok(rows)
}

fn selected_request_query(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_id: &str,
) -> String {
    let node = escape_graphql_string(node_did);
    let session = escape_graphql_string(session_id);
    let request = escape_graphql_string(request_id);
    let requester = requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".to_string());
    format!(
        "query DesktopSelectedRequest {{ {AGENT_REQUEST_NAME}(filter: {{ node_did: {{ _eq: \"{node}\" }}, session_id: {{ _eq: \"{session}\" }}, requester_did: {{ _eq: {requester} }}, purpose: {{ _eq: \"normal\" }}, request_id: {{ _eq: \"{request}\" }} }}) {{ {AGENT_REQUEST_FIELDS} }} }}"
    )
}

struct SelectedRequestLookup {
    query: String,
    expected_doc_id: Option<String>,
    expected_request_id: Option<String>,
}

impl SelectedRequestLookup {
    /// Session metadata can replicate before its physical request; an empty
    /// lookup cannot establish a broken canonical reference.
    fn decode(
        self,
        data: &Value,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
    ) -> Result<Vec<AgentRequestRow>> {
        let selected = snapshot_loaders::complete_request_rows(data, AGENT_REQUEST_NAME)?;
        anyhow::ensure!(
            selected.iter().all(|row| {
                row.node_did.as_deref() == Some(node_did)
                    && row.session_id.as_deref() == Some(session_id)
                    && row.requester_did.as_deref() == requester_did
                    && row.purpose
                        == Some(gents_protocol::request_admission::RequestPurpose::Normal)
                    && self
                        .expected_doc_id
                        .as_deref()
                        .is_none_or(|doc_id| row.doc_id.as_deref() == Some(doc_id))
            }),
            "selected request query returned a row outside its exact node/session/requester scope"
        );
        if selected.is_empty() {
            // A signed client request can arrive after its independently created session.
            return Err(crate::error::BridgeError::new(
                crate::error::BridgeErrorCode::ProjectionPending,
                "selected request has not reached the session owner yet",
            )
            .into());
        }
        if let Some(expected_request_id) = self.expected_request_id.as_deref() {
            anyhow::ensure!(
                selected.len() == 1 && selected[0].request_id == expected_request_id,
                "canonical session head did not resolve to its exact physical request"
            );
        }
        Ok(selected)
    }
}

fn selected_request_lookup(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    observation: Option<&gents_protocol::session::SessionObservation>,
    request_id: Option<&str>,
    fallback_request_id: Option<&str>,
    page_request_doc_ids: &[String],
) -> Result<Option<SelectedRequestLookup>> {
    let lookup = if let Some(request_id) = request_id {
        Some(SelectedRequestLookup {
            query: selected_request_query(node_did, session_id, requester_did, request_id),
            expected_doc_id: None,
            expected_request_id: None,
        })
    } else if let Some(head) =
        observation.and_then(|observation| observation.latest_request.as_ref())
    {
        Some(SelectedRequestLookup {
            query: selected_request_doc_query(
                node_did,
                session_id,
                requester_did,
                &head.request_doc_id,
            ),
            expected_doc_id: Some(head.request_doc_id.clone()),
            expected_request_id: Some(head.request_id.clone()),
        })
    } else if observation.is_none() {
        fallback_request_id.map(|request_id| SelectedRequestLookup {
            query: selected_request_query(node_did, session_id, requester_did, request_id),
            expected_doc_id: None,
            expected_request_id: None,
        })
    } else {
        None
    };
    anyhow::ensure!(
        lookup.is_some() || page_request_doc_ids.is_empty(),
        "session transcript has request owners but the canonical session has no latest request"
    );
    Ok(lookup)
}

fn selected_request_doc_query(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
) -> String {
    let node = escape_graphql_string(node_did);
    let session = escape_graphql_string(session_id);
    let doc_id = escape_graphql_string(request_doc_id);
    let requester = requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".to_string());
    format!(
        "query DesktopSelectedRequestHead {{ {AGENT_REQUEST_NAME}(docIDs: [\"{doc_id}\"], filter: {{ node_did: {{ _eq: \"{node}\" }}, session_id: {{ _eq: \"{session}\" }}, requester_did: {{ _eq: {requester} }}, purpose: {{ _eq: \"normal\" }} }}) {{ {AGENT_REQUEST_FIELDS} }} }}"
    )
}

fn selected_session_query(node_did: &str, session_id: &str) -> String {
    let node = escape_graphql_string(node_did);
    let session = escape_graphql_string(session_id);
    format!(
        "query DesktopSelectedSession {{ {AGENT_SESSION_NAME}(filter: {{ node_did: {{ _eq: \"{node}\" }}, session_id: {{ _eq: \"{session}\" }} }}) {{ {AGENT_SESSION_FIELDS} }} }}"
    )
}

fn selected_session_from_data(
    data: &Value,
    node_did: &str,
    session_id: &str,
) -> Result<AgentSession> {
    let (sessions, complete) = snapshot_loaders::parse_query_rows_with_completeness::<AgentSession>(
        data,
        AGENT_SESSION_NAME,
    )?;
    anyhow::ensure!(complete, "selected session query returned malformed rows");
    anyhow::ensure!(
        sessions.len() == 1,
        "selected session refresh requires exactly one canonical AgentSession row for node/session; found {}",
        sessions.len()
    );
    let session = sessions
        .into_iter()
        .next()
        .expect("one session was checked");
    anyhow::ensure!(
        session.node_did == node_did && session.session_id == session_id,
        "selected session refresh returned a row outside its exact node/session scope"
    );
    Ok(session)
}

fn session_request_seed_query(
    scopes: &[(String, String, Option<String>)],
    page_request_doc_ids: &[String],
) -> String {
    let mut fields = Vec::with_capacity(scopes.len());
    let page_ids = page_request_doc_ids
        .iter()
        .map(String::as_str)
        .filter(|id| !id.trim().is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(",");
    for (index, (node_did, session_id, requester_did)) in scopes.iter().enumerate() {
        let node = escape_graphql_string(node_did);
        let session = escape_graphql_string(session_id);
        let requester = requester_did
            .as_deref()
            .map(|did| format!("\"{}\"", escape_graphql_string(did)))
            .unwrap_or_else(|| "null".to_string());
        let mut predicates = vec![
            "lifecycle_state: { _neq: \"completed\" }".to_string(),
            "lifecycle_state: { _eq: null }".to_string(),
            "retry_parent_request: { _neq: null }".to_string(),
            "retry_parent_request_doc_id: { _neq: null }".to_string(),
            "superseded_by_request_doc_id: { _neq: null }".to_string(),
        ];
        if !page_ids.is_empty() {
            predicates.push(format!("_docID: {{ _in: [{page_ids}] }}"));
        }
        fields.push(format!(
            "seeds_{index}: {AGENT_REQUEST_NAME}(filter: {{ node_did: {{ _eq: \"{node}\" }}, session_id: {{ _eq: \"{session}\" }}, requester_did: {{ _eq: {requester} }}, purpose: {{ _eq: \"normal\" }}, _or: [{}] }}) {{ {AGENT_REQUEST_FIELDS} }}",
            predicates.into_iter().map(|predicate| format!("{{ {predicate} }}")).collect::<Vec<_>>().join(",")
        ));
        fields.push(format!(
            "newest_{index}: {AGENT_REQUEST_NAME}(filter: {{ node_did: {{ _eq: \"{node}\" }}, session_id: {{ _eq: \"{session}\" }}, requester_did: {{ _eq: {requester} }}, purpose: {{ _eq: \"normal\" }} }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}, {{ _docID: DESC }}], limit: 1) {{ {AGENT_REQUEST_FIELDS} }}"
        ));
    }
    format!(
        "query DesktopSessionRequestSeeds {{ {} }}",
        fields.join(" ")
    )
}

fn request_seeds_from_data(
    data: &Value,
    mut selected: Vec<AgentRequestRow>,
    scopes: &[(String, String, Option<String>)],
) -> Result<Vec<AgentRequestRow>> {
    for (index, (node_did, session_id, requester_did)) in scopes.iter().enumerate() {
        let mut rows = snapshot_loaders::complete_request_rows(data, &format!("seeds_{index}"))?;
        rows.extend(snapshot_loaders::complete_request_rows(
            data,
            &format!("newest_{index}"),
        )?);
        anyhow::ensure!(
            rows.iter().all(|row| {
                row.node_did.as_deref() == Some(node_did)
                    && row.session_id.as_deref() == Some(session_id)
                    && row.requester_did == *requester_did
            }),
            "session request seed query returned a row outside its exact node/session/requester scope"
        );
        selected.extend(rows);
    }
    Ok(selected)
}

fn request_seed_scopes(requests: &[AgentRequestRow]) -> Vec<(String, String, Option<String>)> {
    requests
        .iter()
        .filter_map(|row| {
            Some((
                row.node_did.clone()?,
                row.session_id.clone()?,
                row.requester_did.clone(),
            ))
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn request_context_scopes(requests: &[AgentRequestRow]) -> Vec<(String, String)> {
    requests
        .iter()
        .filter_map(|row| Some((row.node_did.clone()?, row.session_id.clone()?)))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn request_context_query(scopes: &[(String, String)]) -> String {
    let mut fields = Vec::with_capacity(scopes.len());
    for (index, (node_did, session_id)) in scopes.iter().enumerate() {
        let node = escape_graphql_string(node_did);
        let session = escape_graphql_string(session_id);
        fields.push(format!(
            "goals_{index}: {GOAL_NAME}(filter: {{ node_did: {{ _eq: \"{node}\" }}, session_id: {{ _eq: \"{session}\" }} }}) {{ {GOAL_FIELDS} }}"
        ));
    }
    format!(
        "query DesktopRequestSessionContext {{ {} }}",
        fields.join(" ")
    )
}

fn request_patch_from_context_data(
    data: &Value,
    requests: Vec<AgentRequestRow>,
    sessions: Vec<AgentSession>,
    requester_did: Option<String>,
    scopes: &[(String, String)],
) -> Result<ClientStore> {
    let mut goals = Vec::new();
    for (index, _) in scopes.iter().enumerate() {
        goals.extend(parse_query_rows::<GoalRow>(
            data,
            &format!("goals_{index}"),
        )?);
    }
    anyhow::ensure!(
        requests
            .iter()
            .all(|row| row.requester_did == requester_did),
        "request closure contains a row outside the canonical session requester scope"
    );
    Ok(ClientStore::from_rows(ClientStoreRows {
        requests,
        sessions,
        goals,
        ..ClientStoreRows::default()
    }))
}

pub(super) async fn execute_local_graphql_query(
    node: &EmbeddedNode,
    query: &str,
    operation: &str,
) -> Result<Value> {
    let response = gents::graphql::graphql_with_transaction_retry(node, query, operation).await?;
    response
        .data
        .with_context(|| format!("{operation} returned no data"))
}

pub(super) async fn load_rows<T>(node: &EmbeddedNode, root: &str, query: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let operation = format!("query for {root}");
    let response = gents::graphql::graphql_with_transaction_retry(node, query, &operation).await?;

    let data = response
        .data
        .with_context(|| format!("query for {root} returned no data"))?;
    parse_query_rows(&data, root)
}

pub(super) async fn execute_access_graphql_query(
    access: &gents::config_client::ConfigAccess,
    query: &str,
    operation: &str,
) -> Result<Value> {
    let body = access
        .execute(query)
        .await
        .with_context(|| operation.to_string())?;
    into_access_graphql_data(body, operation)
}

fn into_access_graphql_data(mut body: Value, operation: &str) -> Result<Value> {
    if let Some(errors) = body.get("errors") {
        if errors.as_array().is_some_and(|rows| !rows.is_empty()) {
            anyhow::bail!("{operation} returned errors: {errors}");
        }
    }
    body.as_object_mut()
        .and_then(|root| root.remove("data"))
        .with_context(|| format!("{operation} returned no data"))
}

pub(super) fn parse_query_rows<T>(data: &Value, root: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let rows = data
        .get(root)
        .ok_or_else(|| anyhow!("query result missing root field {root}"))?;
    parse_row_array(rows, root)
}

pub(super) fn parse_canonical_rows<T>(
    data: &Value,
    root: &str,
    decode: impl Fn(&Value) -> Result<T>,
) -> Result<Vec<T>> {
    let rows = data
        .get(root)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("query result missing canonical row array {root}"))?;
    rows.iter().map(decode).collect()
}

pub(super) fn parse_row_array<T>(rows: &Value, root: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    match rows {
        Value::Null => Ok(Vec::new()),
        Value::Array(rows) => {
            let mut parsed = Vec::with_capacity(rows.len());
            for row in rows {
                match serde_json::from_value(row.clone()) {
                    Ok(row) => parsed.push(row),
                    Err(error) => tracing::warn!(
                        target: "gents_desktop_core::query",
                        root,
                        error = %error,
                        "skipping malformed query row"
                    ),
                }
            }
            Ok(parsed)
        }
        other => Err(anyhow!(
            "query result for {root} returned non-array payload: {other}"
        )),
    }
}

#[cfg(test)]
mod tests {
    mod projection_and_patches;
    mod query_edges;
    mod transcript_pagination;
}
