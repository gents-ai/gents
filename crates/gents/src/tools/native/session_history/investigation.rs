use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use super::inference::{
    aggregate_inference_usage, latest_context_from_detail_rows, INFERENCE_DETAIL_FIELDS,
};
use super::{
    clean, decode, quoted_graphql_list, CompactionRow, InvestigationCallsEnvelope,
    InvestigationEnvelope, SessionCompactionEvent, SessionInvestigationSnapshot,
    SessionRequestEvent, SessionToolCallStats,
};
use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};

pub async fn load_session_investigation(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
) -> Result<SessionInvestigationSnapshot> {
    let node_did = node_did.trim();
    let session_id = session_id.trim();
    if node_did.is_empty() || session_id.is_empty() {
        bail!("sessions get requires a running node DID and non-empty session_id");
    }

    let response = graphql_with_transaction_retry(
        node,
        &session_investigation_query(node_did, session_id),
        "loading session investigation",
    )
    .await?;
    let envelope: InvestigationEnvelope = decode(response.data.as_ref(), "session investigation")?;
    if envelope.requests.is_empty() {
        bail!("session '{session_id}' has no requests owned by this agent");
    }

    let request_doc_ids = envelope
        .requests
        .iter()
        .filter_map(|request| clean(request.doc_id.as_ref()))
        .collect::<Vec<_>>();
    if request_doc_ids.len() != envelope.requests.len() {
        bail!("session investigation contains a request without a physical document ID");
    }
    let response = graphql_with_transaction_retry(
        node,
        &session_investigation_calls_query(node_did, &request_doc_ids),
        "loading session investigation calls",
    )
    .await?;
    let calls: InvestigationCallsEnvelope =
        decode(response.data.as_ref(), "session investigation calls")?;

    let mut audit_usage = Vec::with_capacity(envelope.requests.len());
    for request in &envelope.requests {
        let observation =
            match super::super::title_audit_usage::load_parent_inclusive_audit_usage(node, request)
                .await
            {
                Ok(usage) => super::super::ParentAuditUsageObservation::Available { usage },
                Err(error) => super::super::ParentAuditUsageObservation::Unavailable {
                    parent_request_doc_id: request.doc_id.clone(),
                    parent_request_id: request.request_id.clone(),
                    error: format!("{error:#}"),
                },
            };
        audit_usage.push(observation);
    }
    let mut snapshot = build_session_investigation(node_did, session_id, envelope, calls)?;
    snapshot.parent_inclusive_audit_usage = audit_usage;
    Ok(snapshot)
}

fn session_investigation_query(node_did: &str, session_id: &str) -> String {
    session_investigation_scoped_query(node_did, session_id, None)
}

pub(super) fn session_investigation_scoped_query(
    node_did: &str,
    session_id: &str,
    requester: Option<Option<&str>>,
) -> String {
    let requester_filter = requester
        .map(|did| {
            format!(
                "{{requester_did: {{_eq: {}}}}},",
                did.map(|did| format!("\"{}\"", escape_graphql_string(did)))
                    .unwrap_or_else(|| "null".into())
            )
        })
        .unwrap_or_default();
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    format!(
        r#"{{
            AgentSession(filter: {{ _and: [
                {requester_filter}
                {{ node_did: {{ _eq: "{node_did}" }} }},
                {{ session_id: {{ _eq: "{session_id}" }} }}
            ] }}) {{
                session_id
                agent_id
            }}
            AgentRequest(
                filter: {{ _and: [
                {requester_filter}
                    {{ purpose: {{ _eq: "normal" }} }},
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ session_id: {{ _eq: "{session_id}" }} }}
                ] }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                purpose
                node_did
                requester_did
                agent_id
                session_id
                lifecycle_state
                created_at
                terminalized_at
                failure_reason
                deadline
                retry_count
                caused_by_parent_request_id
            }}
            AgentToolCall(
                filter: {{ _and: [
                {requester_filter}
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ session_id: {{ _eq: "{session_id}" }} }}
                ] }}
            ) {{
                tool_name
                lifecycle_state
                status
                latency_ms
            }}
            CompactionEntry(
                filter: {{ _and: [
                {requester_filter}
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ session_id: {{ _eq: "{session_id}" }} }}
                ] }},
                order: {{ sequence: ASC }}
            ) {{
                request_id
                created_at
                messages_compacted
                compacted_through_sequence
                original_tokens
                compacted_tokens
            }}
            ProviderContextReduction(
                filter: {{ _and: [
                {requester_filter}
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ session_id: {{ _eq: "{session_id}" }} }}
                ] }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                created_at
                messages_compacted
                original_tokens
                compacted_tokens
            }}
            Agent(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{
                agent_id
                inference_profile_id
                context_id
            }}
            AgentContext(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{
                context_id
                compaction_id
            }}
            CompactionConfig(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{
                compaction_id
                strategy
                threshold
            }}
            InferenceProfile(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{
                profile_id
                context_window
            }}
        }}"#
    )
}

pub(super) fn session_investigation_calls_query(
    node_did: &str,
    request_doc_ids: &[String],
) -> String {
    session_investigation_calls_scoped_query(node_did, request_doc_ids, None)
}

pub(super) fn session_investigation_calls_scoped_query(
    node_did: &str,
    request_doc_ids: &[String],
    requester: Option<Option<&str>>,
) -> String {
    let child_scope = requester
        .map(|did| {
            format!(
                "node_did: {{_eq: \"{}\"}}, requester_did: {{_eq: {}}},",
                escape_graphql_string(node_did),
                did.map(|did| format!("\"{}\"", escape_graphql_string(did)))
                    .unwrap_or_else(|| "null".into())
            )
        })
        .unwrap_or_default();
    let node_did = escape_graphql_string(node_did);
    let request_doc_ids = quoted_graphql_list(request_doc_ids);
    format!(
        r#"{{
            InferenceCall(
                filter: {{ _and: [
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ request_doc_id: {{ _in: [{request_doc_ids}] }} }}
                ] }},
                order: {{ queued_at: ASC }}
            ) {{
                {INFERENCE_DETAIL_FIELDS}
            }}
            AgentRequest(
                filter: {{ {child_scope} purpose: {{ _eq: "normal" }}, caused_by_parent_request_doc_id: {{ _in: [{request_doc_ids}] }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                session_id
                created_at
            }}
        }}"#
    )
}

pub(super) fn build_session_investigation(
    node_did: &str,
    session_id: &str,
    envelope: InvestigationEnvelope,
    calls: InvestigationCallsEnvelope,
) -> Result<SessionInvestigationSnapshot> {
    anyhow::ensure!(
        envelope.sessions.len() == 1,
        "session investigation requires one canonical AgentSession"
    );
    let agent_id = clean(envelope.sessions[0].agent_id.as_ref())
        .context("canonical AgentSession is missing agent_id")?;
    let agent = {
        envelope
            .agents
            .iter()
            .find(|agent| agent.agent_id == agent_id)
    };
    let context = agent
        .and_then(|agent| agent.context_id.as_deref())
        .and_then(|context_id| {
            envelope
                .contexts
                .iter()
                .find(|context| context.context_id == context_id)
        });
    let compaction = context
        .and_then(|context| context.compaction_id.as_deref())
        .and_then(|compaction_id| {
            envelope
                .compactions_config
                .iter()
                .find(|config| config.compaction_id == compaction_id)
        });
    let profile = agent
        .and_then(|agent| agent.inference_profile_id.as_deref())
        .and_then(|profile_id| {
            envelope
                .profiles
                .iter()
                .find(|profile| profile.profile_id == profile_id)
        });
    let parent_request_ids = envelope
        .requests
        .iter()
        .filter_map(|request| clean(request.caused_by_parent_request_id.as_ref()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let requests = envelope
        .requests
        .iter()
        .map(|request| SessionRequestEvent {
            request_id: request.request_id.trim().to_string(),
            lifecycle_state: request
                .lifecycle_state
                .map(|state| state.as_str().to_string()),
            created_at: clean(request.created_at.as_ref()),
            terminalized_at: clean(request.terminalized_at.as_ref()),
            failure_reason: clean(request.failure_reason.as_ref()),
            deadline: clean(request.deadline.as_ref()),
            retry_count: request.retry_count,
        })
        .collect();

    let mut tool_calls = SessionToolCallStats::default();
    for call in envelope.tool_calls {
        tool_calls.total += 1;
        let tool = clean(call.tool_name.as_ref()).unwrap_or_else(|| "unknown".to_string());
        *tool_calls.by_tool.entry(tool).or_default() += 1;
        let status = clean(call.lifecycle_state.as_ref())
            .or_else(|| clean(call.status.as_ref()))
            .unwrap_or_else(|| "unknown".to_string());
        *tool_calls.by_status.entry(status).or_default() += 1;
        tool_calls.total_latency_ms = tool_calls
            .total_latency_ms
            .saturating_add(call.latency_ms.unwrap_or_default().max(0));
    }

    let token_usage = aggregate_inference_usage(&calls.inference_calls)?;
    let latest_context = latest_context_from_detail_rows(&calls.inference_calls)?;
    let mut compactions = envelope
        .compactions
        .into_iter()
        .map(|row| compaction_event("session_prefix", row))
        .chain(
            envelope
                .provider_reductions
                .into_iter()
                .map(|row| compaction_event("provider_context", row)),
        )
        .collect::<Vec<_>>();
    compactions.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    let child_session_ids = calls
        .child_requests
        .iter()
        .filter_map(|request| clean(request.session_id.as_ref()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(SessionInvestigationSnapshot {
        node_did: node_did.to_string(),
        session_id: session_id.to_string(),
        requests,
        tool_calls,
        token_usage,
        parent_inclusive_audit_usage: Vec::new(),
        compactions,
        latest_context: latest_context.clone(),
        compaction_strategy: compaction
            .and_then(|config| config.strategy.as_ref())
            .cloned()
            .unwrap_or_default(),
        compaction_threshold: latest_context
            .as_ref()
            .map(|context| {
                f64::from(
                    u32::try_from(context.accounting.compaction_threshold_basis_points)
                        .unwrap_or(u32::MAX),
                ) / 10_000.0
            })
            .or_else(|| compaction.and_then(|config| config.threshold))
            .or(Some(
                crate::inference::resolved::DEFAULT_COMPACTION_THRESHOLD,
            )),
        context_window: latest_context
            .as_ref()
            .and_then(|context| i64::try_from(context.accounting.context_window).ok())
            .or_else(|| profile.and_then(|profile| profile.context_window)),
        parent_request_ids,
        child_session_ids,
    })
}

fn compaction_event(scope: &str, row: CompactionRow) -> SessionCompactionEvent {
    SessionCompactionEvent {
        scope: scope.to_string(),
        created_at: clean(row.created_at.as_ref()),
        messages_compacted: row.messages_compacted,
        original_tokens: row.original_tokens,
        compacted_tokens: row.compacted_tokens,
        compacted_through_sequence: row.compacted_through_sequence,
        request_id: clean(row.request_id.as_ref()),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ToolCallDetailRow {
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct AgentDetailRow {
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    inference_profile_id: Option<String>,
    #[serde(default)]
    context_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ContextDetailRow {
    #[serde(default)]
    context_id: String,
    #[serde(default)]
    compaction_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CompactionConfigDetailRow {
    #[serde(default)]
    compaction_id: String,
    #[serde(default)]
    strategy: Option<crate::inference::compaction::CompactionStrategy>,
    #[serde(default)]
    threshold: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ProfileDetailRow {
    #[serde(default)]
    profile_id: String,
    #[serde(default)]
    context_window: Option<i64>,
}
