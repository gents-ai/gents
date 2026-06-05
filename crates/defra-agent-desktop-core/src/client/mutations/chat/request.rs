use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_agent_protocol::row::AgentRequestRow;
use defra_node::EmbeddedNode;
use uuid::Uuid;

use crate::client::store::ClientStore;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, execute_remote_mutation, normalize_optional_string,
    normalize_required,
};
use super::binding::resolve_agent_binding;
use super::conversation::{build_upsert_conversation_field, build_upsert_session_field};

const DEFAULT_REQUEST_MAX_RETRIES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedRequest {
    pub request_id: String,
    pub session_id: String,
    pub agent_did: String,
    pub behavior_id: Option<String>,
}

/// Optional submission-time controls. All fields default to "unset"; the
/// caller opts in to TTL enforcement or retry threading by populating them.
#[derive(Debug, Clone, Default)]
pub struct SubmitRequestOptions {
    /// When set, written to the request's `valid_until` field. The runtime's
    /// admission/scheduler layers treat requests past this deadline as `Stale`.
    /// None means no TTL is recorded on this row.
    pub valid_until: Option<DateTime<Utc>>,
    /// When this submission is a resend (or otherwise links to a prior
    /// request), the parent request id is threaded into `retry_parent_request`
    /// and the parent's retry root is carried forward into `retry_root_request`.
    pub retry_parent_request: Option<String>,
    /// Sampling override: if set, written to the request's `temperature` field.
    pub temperature: Option<f64>,
    /// Sampling override: if set, written to the request's `top_p` field.
    pub top_p: Option<f64>,
    /// Sampling override: if set, written to the request's `top_k` field.
    pub top_k: Option<i64>,
    /// Sampling override: if set, written to the request's `max_tokens` field.
    pub max_tokens: Option<i64>,
    /// Free-form metadata attached to the request (submitter-defined JSON/string).
    pub metadata: Option<String>,
}

pub async fn submit_request(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
    content: &str,
    behavior_id: Option<&str>,
    options: SubmitRequestOptions,
) -> Result<SubmittedRequest> {
    let session_id = normalize_required("session_id", session_id)?;
    let agent_did = normalize_required("agent_did", agent_did)?;
    let content = normalize_required("content", content)?;
    let request_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let binding = resolve_agent_binding(store, agent_did, behavior_id, Some(session_id))?;

    // Thread retry linkage: carry parent's retry root forward, else this row is
    // the root of its own retry chain.
    let (retry_parent_request, retry_root_request) =
        if let Some(parent_id) = options.retry_parent_request.as_deref() {
            let root = fetch_retry_root(node, parent_id)
                .await?
                .unwrap_or_else(|| parent_id.to_string());
            (parent_id.to_string(), root)
        } else {
            (String::new(), request_id.clone())
        };

    let session_field = build_upsert_session_field(
        "session",
        store,
        session_id,
        &binding.agent_name,
        &binding.behavior_id,
        &created_at,
    );
    let request_field = build_add_agent_request_field(
        "request",
        &request_id,
        agent_did,
        binding.behavior_id.as_deref().unwrap_or(""),
        session_id,
        &retry_parent_request,
        &retry_root_request,
        content,
        &created_at,
        0,
        i64::from(DEFAULT_REQUEST_MAX_RETRIES),
        "",
        "interactive",
        &submit_request_extra_fields(&options),
    );
    let conversation_field = build_upsert_conversation_field(
        "conversation",
        store,
        session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
        &request_id,
        content,
        "active",
        &created_at,
    );
    let mutation =
        build_coalesced_submit_mutation(&[session_field, request_field, conversation_field]);
    execute_mutation(node, &mutation, "submit_request").await?;

    Ok(SubmittedRequest {
        request_id,
        session_id: session_id.to_string(),
        agent_did: agent_did.to_string(),
        behavior_id: binding.behavior_id,
    })
}

pub async fn submit_request_to_graphql(
    graphql: &str,
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
    content: &str,
    behavior_id: Option<&str>,
    options: SubmitRequestOptions,
) -> Result<SubmittedRequest> {
    let graphql = normalize_required("graphql", graphql)?;
    let session_id = normalize_required("session_id", session_id)?;
    let agent_did = normalize_required("agent_did", agent_did)?;
    let content = normalize_required("content", content)?;
    if options.retry_parent_request.is_some() {
        bail!("remote GraphQL chat submission does not yet support retry threading");
    }

    let request_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let binding = resolve_agent_binding(store, agent_did, behavior_id, Some(session_id))?;
    let session_field = build_upsert_session_field(
        "session",
        store,
        session_id,
        &binding.agent_name,
        &binding.behavior_id,
        &created_at,
    );
    let request_field = build_add_agent_request_field(
        "request",
        &request_id,
        agent_did,
        binding.behavior_id.as_deref().unwrap_or(""),
        session_id,
        "",
        &request_id,
        content,
        &created_at,
        0,
        i64::from(DEFAULT_REQUEST_MAX_RETRIES),
        "",
        "interactive",
        &submit_request_extra_fields(&options),
    );
    let conversation_field = build_upsert_conversation_field(
        "conversation",
        store,
        session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
        &request_id,
        content,
        "active",
        &created_at,
    );
    let mutation =
        build_coalesced_submit_mutation(&[session_field, request_field, conversation_field]);
    execute_remote_mutation(graphql, &mutation, "submit_request").await?;

    Ok(SubmittedRequest {
        request_id,
        session_id: session_id.to_string(),
        agent_did: agent_did.to_string(),
        behavior_id: binding.behavior_id,
    })
}

pub async fn retry_request(
    node: &EmbeddedNode,
    store: &ClientStore,
    parent: &AgentRequestRow,
) -> Result<SubmittedRequest> {
    retry_request_with_request_id(node, store, parent, Uuid::new_v4().to_string()).await
}

async fn retry_request_with_request_id(
    node: &EmbeddedNode,
    store: &ClientStore,
    parent: &AgentRequestRow,
    request_id: String,
) -> Result<SubmittedRequest> {
    let parent_request_id = normalize_required("request_id", &parent.request_id)?;
    let request_id = normalize_required("new_request_id", &request_id)?.to_string();
    let parent_session_id = normalize_required(
        "session_id",
        parent
            .session_id
            .as_deref()
            .context("retry parent request must have a session_id")?,
    )?;
    let agent_did = normalize_required(
        "agent_did",
        parent
            .agent_did
            .as_deref()
            .context("retry parent request must have an agent_did")?,
    )?;
    let content = normalize_required(
        "content",
        parent
            .content
            .as_deref()
            .context("retry parent request must have content")?,
    )?;
    let behavior_id = normalize_optional_string(parent.behavior_id.as_deref());
    let retry_root_request = normalize_optional_string(parent.retry_root_request.as_deref())
        .unwrap_or(parent_request_id);
    let retry_count = parent.retry_count.unwrap_or_default() + 1;
    let max_retries = parent
        .max_retries
        .unwrap_or(i64::from(DEFAULT_REQUEST_MAX_RETRIES));
    let backend_id = normalize_optional_string(parent.backend_id.as_deref()).unwrap_or("");
    let execution_origin = match normalize_optional_string(parent.execution_origin.as_deref()) {
        Some(origin) => origin,
        None => {
            tracing::debug!(
                request_id = %parent_request_id,
                "retry parent request missing execution_origin; defaulting retry origin to interactive"
            );
            "interactive"
        }
    };
    ensure_retry_parent_eligible(parent, retry_count - 1, max_retries)?;
    ensure_retry_parent_request_exists(node, parent_request_id).await?;
    // Lean checks `isLatest` inside one session-state transition. The desktop
    // GraphQL API does not expose a transactional conditional create+update
    // primitive here, so Rust enforces the same predicate as a database-backed
    // preflight immediately before the coalesced write.
    ensure_latest_retry_parent(node, parent_session_id, parent_request_id).await?;
    // Lean also requires the new request id to be fresh. This preflight catches
    // injected-id tests and UUID collisions; a schema uniqueness constraint is
    // still the hard concurrent guarantee.
    ensure_new_retry_request_id_available(node, &request_id).await?;
    let session_id = parent_session_id.to_string();
    let created_at = Utc::now().to_rfc3339();
    let binding = resolve_agent_binding(store, agent_did, behavior_id, Some(parent_session_id))?;

    let session_field = build_upsert_session_field(
        "session",
        store,
        &session_id,
        &binding.agent_name,
        &binding.behavior_id,
        &created_at,
    );
    let request_field = build_add_agent_request_field(
        "request",
        &request_id,
        agent_did,
        binding.behavior_id.as_deref().unwrap_or(""),
        &session_id,
        parent_request_id,
        retry_root_request,
        content,
        &created_at,
        retry_count,
        max_retries,
        backend_id,
        execution_origin,
        &submit_request_extra_fields(&SubmitRequestOptions {
            temperature: parent.temperature,
            top_p: parent.top_p,
            top_k: parent.top_k,
            max_tokens: parent.max_tokens,
            metadata: parent.metadata.clone(),
            ..SubmitRequestOptions::default()
        }),
    );
    let conversation_field = build_upsert_conversation_field(
        "conversation",
        store,
        &session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
        &request_id,
        content,
        "active",
        &created_at,
    );
    let mutation =
        build_coalesced_submit_mutation(&[session_field, request_field, conversation_field]);
    execute_mutation(node, &mutation, "retry_request").await?;

    Ok(SubmittedRequest {
        request_id,
        session_id,
        agent_did: agent_did.to_string(),
        behavior_id: binding.behavior_id,
    })
}

fn ensure_retry_parent_eligible(
    parent: &AgentRequestRow,
    parent_retry_count: i64,
    max_retries: i64,
) -> Result<()> {
    // The Lean `.released` admission predicate is not persisted on
    // `AgentRequestRow`; on this desktop surface it is represented by requiring
    // the parent to be terminal failed/error. Non-terminal rows, including rows
    // still waiting on admission, fail this lifecycle/status gate.
    let lifecycle_state = normalize_required(
        "lifecycle_state",
        parent
            .lifecycle_state
            .as_deref()
            .context("retry parent request must have a lifecycle_state")?,
    )?;
    let status = normalize_required(
        "status",
        parent
            .status
            .as_deref()
            .context("retry parent request must have a status")?,
    )?;

    if lifecycle_state != "failed" || status != "error" {
        bail!(
            "retry parent request must be failed/error, got lifecycle_state={lifecycle_state} status={status}"
        );
    }
    if parent_retry_count >= max_retries {
        bail!(
            "retry parent request exhausted retry budget: retry_count={parent_retry_count} max_retries={max_retries}"
        );
    }
    if let Some(deadline) = normalize_optional_string(parent.deadline.as_deref()) {
        let deadline = DateTime::parse_from_rfc3339(deadline)
            .with_context(|| format!("retry parent request has invalid deadline {deadline:?}"))?
            .with_timezone(&Utc);
        if Utc::now() > deadline {
            bail!(
                "retry parent request deadline is closed: deadline={}",
                deadline.to_rfc3339()
            );
        }
    }

    Ok(())
}

async fn ensure_retry_parent_request_exists(
    node: &EmbeddedNode,
    parent_request_id: &str,
) -> Result<()> {
    let escaped_request_id = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        bail!("querying retry parent request failed: {:?}", resp.errors);
    }
    let exists = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .is_some_and(|rows| !rows.is_empty());

    if !exists {
        bail!("retry parent request not found: request_id={parent_request_id}");
    }

    Ok(())
}

async fn ensure_latest_retry_parent(
    node: &EmbeddedNode,
    session_id: &str,
    parent_request_id: &str,
) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                limit: 1
            ) {{ latest_request_id }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        bail!(
            "querying retry parent conversation failed: {:?}",
            resp.errors
        );
    }
    let latest_request_id = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentConversation"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("latest_request_id"))
        .and_then(|value| value.as_str());

    let Some(latest_request_id) = latest_request_id else {
        bail!("retry parent conversation not found for session {session_id}");
    };
    let Some(latest_request_id) = normalize_optional_string(Some(latest_request_id)) else {
        bail!("retry parent conversation for session {session_id} has no latest_request_id");
    };

    if latest_request_id != parent_request_id {
        bail!(
            "retry parent request must be latest for session {session_id}, got latest_request_id={latest_request_id}"
        );
    }

    Ok(())
}

async fn ensure_new_retry_request_id_available(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<()> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        bail!(
            "querying retry request id availability failed: {:?}",
            resp.errors
        );
    }
    let exists = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .is_some_and(|rows| !rows.is_empty());

    if exists {
        bail!("retry new request id already exists: request_id={request_id}");
    }

    Ok(())
}

fn submit_request_extra_fields(options: &SubmitRequestOptions) -> String {
    let valid_until_field = match options.valid_until.as_ref() {
        Some(valid_until) => {
            let escaped = escape_graphql_string(&valid_until.to_rfc3339());
            format!(
                r#",
                valid_until: "{escaped}""#,
            )
        }
        None => String::new(),
    };

    // Only emit sampling override + metadata fields when the caller actually
    // set them. Omitting a field leaves the schema default (null) in place;
    // emitting `null` explicitly also works but leaving the field out keeps
    // the mutation shape identical to what previously-submitted rows used
    // before the override plumbing landed.
    let mut override_parts: Vec<String> = Vec::new();
    if let Some(temperature) = options.temperature {
        override_parts.push(format!("temperature: {temperature}"));
    }
    if let Some(top_p) = options.top_p {
        override_parts.push(format!("top_p: {top_p}"));
    }
    if let Some(top_k) = options.top_k {
        override_parts.push(format!("top_k: {top_k}"));
    }
    if let Some(max_tokens) = options.max_tokens {
        override_parts.push(format!("max_tokens: {max_tokens}"));
    }
    if let Some(metadata) = options.metadata.as_deref() {
        override_parts.push(format!(
            r#"metadata: "{}""#,
            escape_graphql_string(metadata)
        ));
    }
    let override_fields = if override_parts.is_empty() {
        String::new()
    } else {
        format!(
            ",\n                {}",
            override_parts.join(",\n                ")
        )
    };

    format!("{valid_until_field}{override_fields}")
}

#[allow(clippy::too_many_arguments)]
fn build_add_agent_request_field(
    alias: &str,
    request_id: &str,
    agent_did: &str,
    behavior_id: &str,
    session_id: &str,
    retry_parent_request: &str,
    retry_root_request: &str,
    content: &str,
    created_at: &str,
    retry_count: i64,
    max_retries: i64,
    backend_id: &str,
    execution_origin: &str,
    extra_fields: &str,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_retry_parent = escape_graphql_string(retry_parent_request);
    let escaped_retry_root = escape_graphql_string(retry_root_request);
    let escaped_content = escape_graphql_string(content);
    let escaped_created_at = escape_graphql_string(created_at);
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_execution_origin = escape_graphql_string(execution_origin);

    format!(
        r#"{alias}: add_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "{escaped_retry_parent}",
                retry_root_request: "{escaped_retry_root}",
                superseded_by_request: "",
                content: "{escaped_content}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "{escaped_backend_id}",
                execution_origin: "{escaped_execution_origin}",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: {retry_count},
                max_retries: {max_retries}{extra_fields}
            }}) {{ _docID }}
        "#
    )
}

fn build_coalesced_submit_mutation(fields: &[String; 3]) -> String {
    format!(
        "mutation {{\n{}\n{}\n{}\n}}",
        fields[0], fields[1], fields[2]
    )
}

/// Resend a stale-terminal request by reading its inputs and submitting a
/// fresh row whose `retry_parent_request` points back at the stale one.
/// Only `lifecycle_state=dead` with `failure_reason=Stale` is eligible — any
/// other state would risk bypassing legitimate terminal classifications.
pub async fn resend_request(
    node: &EmbeddedNode,
    store: &ClientStore,
    stale_request_id: &str,
) -> Result<SubmittedRequest> {
    let stale = fetch_request_view(node, stale_request_id).await?;
    if stale.lifecycle_state != "dead" || stale.failure_reason != "Stale" {
        anyhow::bail!(
            "request {stale_request_id} is not a stale terminal (lifecycle_state={}, failure_reason={})",
            stale.lifecycle_state,
            stale.failure_reason
        );
    }
    let retry_session_id = Uuid::new_v4().to_string();
    submit_request(
        node,
        store,
        &retry_session_id,
        &stale.agent_did,
        &stale.content,
        stale.behavior_id.as_deref(),
        SubmitRequestOptions {
            valid_until: Some(Utc::now() + chrono::Duration::minutes(5)),
            retry_parent_request: Some(stale_request_id.to_string()),
            // Preserve sampling overrides + metadata from the stale row.
            // Dropping them would silently change model behavior on retry.
            temperature: stale.temperature,
            top_p: stale.top_p,
            top_k: stale.top_k,
            max_tokens: stale.max_tokens,
            metadata: stale.metadata.clone(),
        },
    )
    .await
}

/// Minimal projection of an AgentRequest used by resend to copy over inputs.
/// Carries sampling overrides + metadata so resend preserves submitter intent.
struct StaleRequestView {
    agent_did: String,
    behavior_id: Option<String>,
    content: String,
    lifecycle_state: String,
    failure_reason: String,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    max_tokens: Option<i64>,
    metadata: Option<String>,
}

async fn fetch_request_view(node: &EmbeddedNode, request_id: &str) -> Result<StaleRequestView> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                agent_did
                behavior_id
                content
                lifecycle_state
                failure_reason
                temperature
                top_p
                top_k
                max_tokens
                metadata
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("fetch_request({request_id}) failed: {:?}", resp.errors);
    }
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))?;
    Ok(StaleRequestView {
        agent_did: row
            .get("agent_did")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        behavior_id: row
            .get("behavior_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
        content: row
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        lifecycle_state: row
            .get("lifecycle_state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        failure_reason: row
            .get("failure_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        temperature: row.get("temperature").and_then(|v| v.as_f64()),
        top_p: row.get("top_p").and_then(|v| v.as_f64()),
        top_k: row.get("top_k").and_then(|v| v.as_i64()),
        max_tokens: row.get("max_tokens").and_then(|v| v.as_i64()),
        metadata: row
            .get("metadata")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
    })
}

async fn fetch_retry_root(node: &EmbeddedNode, request_id: &str) -> Result<Option<String>> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                retry_root_request
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("fetch_retry_root({request_id}) failed: {:?}", resp.errors);
    }
    Ok(resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("retry_root_request"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from))
}

#[cfg(test)]
#[path = "../../../../../defra-agent/src/lean_vocab_test.rs"]
mod lean_vocab_test;

#[cfg(test)]
mod tests {
    use anyhow::{bail, Context, Result};
    use serde::Deserialize;

    use super::*;
    use crate::client::{ClientCore, ClientCoreOptions, DesktopPaths};

    use super::lean_vocab_test::{
        assert_lean_transition_is_legal, lean_contract_snapshot, LeanSessionRecoveryCase,
    };

    const RECOVERY_AGENT_DID: &str = "did:defra:amy";
    const RECOVERY_BEHAVIOR_ID: &str = "amy-code";

    #[derive(Debug)]
    struct RecoveryPreState {
        session_id: String,
        failed_request_id: String,
        existing_request_id: Option<String>,
        pre_latest_request_id: String,
        parent: AgentRequestRow,
    }

    #[derive(Debug)]
    struct ForcedRequestState {
        status: &'static str,
        lifecycle_state: String,
        retry_count: i64,
        max_retries: i64,
        deadline: String,
        backend_id: String,
        execution_origin: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RetryRequestIdInjection {
        new_request_id: String,
    }

    #[derive(Debug, Deserialize)]
    struct RecoveryRequestRow {
        request_id: String,
        agent_did: String,
        behavior_id: String,
        session_id: String,
        content: String,
        temperature: Option<f64>,
        top_p: Option<f64>,
        top_k: Option<i64>,
        max_tokens: Option<i64>,
        metadata: Option<String>,
        status: String,
        lifecycle_state: String,
        backend_id: String,
        execution_origin: String,
        retry_root_request: String,
        retry_parent_request: String,
        retry_count: i64,
        max_retries: i64,
    }

    #[derive(Debug, Deserialize)]
    struct RecoveryConversationRow {
        latest_request_id: String,
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn generated_session_recovery_cases_drive_desktop_retry_request() -> Result<()> {
        let cases = &lean_contract_snapshot().session_recovery_cases;
        let legal_count = cases.iter().filter(|case| case.legal).count();
        let illegal_count = cases.len() - legal_count;
        assert_eq!(
            (legal_count, illegal_count),
            (3, 15),
            "Lean SessionRecovery case split changed; update this desktop driver before bumping"
        );

        let tempdir = tempfile::tempdir()?;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        )
        .await?;

        let result = async {
            for case in cases {
                assert_eq!(case.action.as_str(), "reissueFailed");
                if case.legal {
                    assert_lean_transition_is_legal(
                        "SessionRecovery",
                        &case.pre_latest_state,
                        &case.post_latest_state,
                    );
                } else {
                    assert!(
                        case.post_latest_state.is_empty(),
                        "illegal Lean case {} must not carry a post latest state",
                        case.name
                    );
                }

                drive_session_recovery_case_with_core(&core, case)
                    .await
                    .with_context(|| format!("driving Lean SessionRecovery case {}", case.name))?;
            }

            Ok::<(), anyhow::Error>(())
        }
        .await;
        let shutdown = core.shutdown().await;
        result?;
        shutdown?;
        Ok(())
    }

    async fn drive_session_recovery_case_with_core(
        core: &ClientCore,
        case: &LeanSessionRecoveryCase,
    ) -> Result<()> {
        let pre = seed_session_recovery_pre_state(core, case).await?;
        let pre_count = request_count_for_session_for_test(core.node(), &pre.session_id).await?;
        assert_eq!(
            pre_count, case.pre_request_count,
            "pre request count must match Lean witness for {}",
            case.name
        );
        assert_eq!(
            latest_request_id_for_session_for_test(core.node(), &pre.session_id).await?,
            pre.pre_latest_request_id,
            "pre latest request role must match Lean witness for {}",
            case.name
        );
        if case.pre_latest_exists {
            assert_eq!(
                fetch_request_row_for_test(core.node(), &pre.pre_latest_request_id)
                    .await?
                    .lifecycle_state,
                case.pre_latest_state,
                "pre latest request state must match Lean witness for {}",
                case.name
            );
        } else {
            assert_eq!(
                request_count_by_id_for_test(core.node(), &pre.pre_latest_request_id).await?,
                0,
                "missing latest request must be absent for {}",
                case.name
            );
        }
        if case.pre_failed_exists {
            assert_eq!(
                pre.parent.lifecycle_state.as_deref(),
                Some(case.pre_failed_state.as_str()),
                "pre failed request state must match Lean witness for {}",
                case.name
            );
        }
        assert_eq!(
            pre.parent.retry_count,
            Some(case.pre_retry_count as i64),
            "pre retry_count must match Lean witness for {}",
            case.name
        );
        assert_eq!(
            pre.parent.max_retries,
            Some(case.max_retries as i64),
            "pre max_retries must match Lean witness for {}",
            case.name
        );
        assert_eq!(
            pre.parent.backend_id.as_deref(),
            Some(case.pre_backend.as_str()),
            "pre backend_id must match Lean witness for {}",
            case.name
        );
        assert_eq!(
            pre.parent.execution_origin.as_deref(),
            Some(case.pre_origin.as_str()),
            "pre execution_origin must match Lean witness for {}",
            case.name
        );

        let injected_new_request_id = injected_new_request_id(case, &pre)?;
        let result = match injected_new_request_id.clone() {
            Some(injection) => {
                retry_request_with_id_injection_for_test(core, &pre.parent, injection).await
            }
            None => core.retry_request(&pre.parent).await,
        };

        if case.legal {
            let submitted = result?;
            assert_legal_session_recovery_post_state(core, case, &pre, &submitted.request_id).await
        } else {
            assert_illegal_session_recovery_post_state(
                core,
                case,
                &pre,
                result.unwrap_err().to_string(),
                injected_new_request_id
                    .as_ref()
                    .map(|injection| injection.new_request_id.as_str()),
            )
            .await
        }
    }

    async fn seed_session_recovery_pre_state(
        core: &ClientCore,
        case: &LeanSessionRecoveryCase,
    ) -> Result<RecoveryPreState> {
        let created = core
            .create_conversation(RECOVERY_AGENT_DID, Some(RECOVERY_BEHAVIOR_ID))
            .await?;
        let failed_is_latest = case.pre_latest_id == case.failed_id;
        let should_seed_failed =
            case.pre_request_ids.contains(&case.failed_id) || !case.pre_failed_exists;
        let should_seed_existing = case
            .pre_request_ids
            .iter()
            .any(|request_id| *request_id != case.failed_id);

        let mut failed = None;
        let mut existing = None;
        if failed_is_latest {
            if should_seed_existing {
                existing = Some(
                    submit_recovery_seed_request(core, &created.session_id, case, "existing")
                        .await?,
                );
            }
            if should_seed_failed {
                failed = Some(
                    submit_recovery_seed_request(core, &created.session_id, case, "failed").await?,
                );
            }
        } else {
            if should_seed_failed {
                failed = Some(
                    submit_recovery_seed_request(core, &created.session_id, case, "failed").await?,
                );
            }
            if should_seed_existing {
                existing = Some(
                    submit_recovery_seed_request(core, &created.session_id, case, "latest").await?,
                );
            }
        }

        let failed_request_id = failed
            .as_ref()
            .map(|request| request.request_id.clone())
            .unwrap_or_else(|| format!("missing-failed-{}", case.name));
        let existing_request_id = existing.as_ref().map(|request| request.request_id.clone());
        let pre_latest_request_id = if case.pre_latest_exists {
            if failed_is_latest {
                failed_request_id.clone()
            } else {
                existing_request_id.clone().with_context(|| {
                    format!(
                        "Lean case {} expected an existing latest request",
                        case.name
                    )
                })?
            }
        } else {
            failed_request_id.clone()
        };

        let expected_parent_status = retry_parent_status_for_case(case);
        if let Some(failed) = failed.as_ref() {
            force_request_state_for_test(
                core.node(),
                &failed.request_id,
                &forced_retry_parent_state(case),
            )
            .await?;
        }
        if let Some(existing) = existing.as_ref() {
            if !failed_is_latest {
                force_request_state_for_test(
                    core.node(),
                    &existing.request_id,
                    &forced_latest_request_state(case),
                )
                .await?;
            }
        }
        if !case.pre_failed_exists {
            delete_request_by_id_for_test(core.node(), &failed_request_id).await?;
        }
        core.refresh_store().await?;

        let parent = if case.pre_failed_exists {
            let parent = request_from_store_for_test(core, &failed_request_id)?;
            assert_eq!(
                parent.lifecycle_state.as_deref(),
                Some(case.pre_failed_state.as_str()),
                "seeded retry parent lifecycle must match Lean witness for {}",
                case.name
            );
            assert_eq!(
                parent.status.as_deref(),
                Some(expected_parent_status),
                "seeded retry parent admission/status must match Lean witness for {}",
                case.name
            );
            assert_eq!(
                parent.backend_id.as_deref(),
                Some(case.pre_backend.as_str()),
                "seeded retry parent backend_id did not refresh into the desktop store for {}",
                case.name
            );
            assert_eq!(
                parent.execution_origin.as_deref(),
                Some(case.pre_origin.as_str()),
                "seeded retry parent execution_origin did not refresh into the desktop store for {}",
                case.name
            );
            parent
        } else {
            synthetic_missing_retry_parent(case, &created.session_id, &failed_request_id)
        };

        Ok(RecoveryPreState {
            session_id: created.session_id,
            failed_request_id,
            existing_request_id,
            pre_latest_request_id,
            parent,
        })
    }

    async fn submit_recovery_seed_request(
        core: &ClientCore,
        session_id: &str,
        case: &LeanSessionRecoveryCase,
        role: &str,
    ) -> Result<SubmittedRequest> {
        core.submit_request(
            session_id,
            RECOVERY_AGENT_DID,
            &format!("{role} request for {}", case.name),
            None,
        )
        .await
    }

    fn synthetic_missing_retry_parent(
        case: &LeanSessionRecoveryCase,
        session_id: &str,
        request_id: &str,
    ) -> AgentRequestRow {
        AgentRequestRow {
            request_id: request_id.to_string(),
            agent_did: Some(RECOVERY_AGENT_DID.to_string()),
            behavior_id: Some(RECOVERY_BEHAVIOR_ID.to_string()),
            session_id: Some(session_id.to_string()),
            retry_parent_request: Some(String::new()),
            retry_root_request: Some(request_id.to_string()),
            superseded_by_request: Some(String::new()),
            content: Some(format!("missing request for {}", case.name)),
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            metadata: None,
            status: Some(retry_parent_status_for_case(case).to_string()),
            lifecycle_state: Some(case.pre_failed_state.clone()),
            backend_id: Some(case.pre_backend.clone()),
            execution_origin: Some(case.pre_origin.clone()),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            caused_by_parent_request_id: None,
            failure_reason: Some(String::new()),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            claimed_at: None,
            deadline: Some(recovery_deadline_for_case(case)),
            retry_count: Some(case.pre_retry_count as i64),
            max_retries: Some(case.max_retries as i64),
            interrupt_requested_at: None,
            valid_until: None,
        }
    }

    #[derive(Debug, Deserialize)]
    struct DocIdForTest {
        #[serde(rename = "_docID")]
        doc_id: String,
    }

    async fn delete_request_by_id_for_test(node: &EmbeddedNode, request_id: &str) -> Result<()> {
        let escaped_request_id = escape_graphql_string(request_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    limit: 1
                ) {{ _docID }}
            }}"#
        );
        let row: DocIdForTest = query_single_for_test(node, &query, "AgentRequest").await?;
        let escaped_doc_id = escape_graphql_string(&row.doc_id);
        let mutation = format!(
            r#"mutation {{
                delete_AgentRequest(docID: "{escaped_doc_id}") {{ _docID }}
            }}"#
        );
        let resp = node.execute(&mutation).await;
        if resp.has_errors() {
            bail!("delete request {request_id} failed: {:?}", resp.errors);
        }
        Ok(())
    }

    async fn assert_legal_session_recovery_post_state(
        core: &ClientCore,
        case: &LeanSessionRecoveryCase,
        pre: &RecoveryPreState,
        new_request_id: &str,
    ) -> Result<()> {
        assert_eq!(case.pre_request_count + 1, case.post_request_count);
        assert_eq!(
            request_count_for_session_for_test(core.node(), &pre.session_id).await?,
            case.post_request_count,
            "post request count must match Lean witness for {}",
            case.name
        );
        assert_eq!(
            latest_request_id_for_session_for_test(core.node(), &pre.session_id).await?,
            new_request_id,
            "new request must become latest for {}",
            case.name
        );
        assert_eq!(
            core.store().focused_request_id(),
            Some(new_request_id.to_string())
        );

        let new_request = fetch_request_row_for_test(core.node(), new_request_id).await?;
        assert_eq!(new_request.request_id, new_request_id);
        assert_eq!(new_request.session_id, pre.session_id);
        assert_eq!(new_request.agent_did, RECOVERY_AGENT_DID);
        assert_eq!(new_request.behavior_id, RECOVERY_BEHAVIOR_ID);
        assert_eq!(
            new_request.content,
            pre.parent.content.as_deref().unwrap_or_default()
        );
        assert_eq!(new_request.status, "pending");
        assert_eq!(new_request.lifecycle_state, case.post_new_state);
        assert_eq!(new_request.retry_parent_request, pre.failed_request_id);
        assert_eq!(new_request.retry_root_request, pre.failed_request_id);
        assert_eq!(new_request.retry_count, case.post_retry_count as i64);
        assert_eq!(new_request.max_retries, case.max_retries as i64);
        if case.origin_preserved {
            assert_eq!(new_request.execution_origin, case.post_new_origin);
        }
        if case.backend_preserved {
            assert_eq!(new_request.backend_id, case.post_new_backend);
        }

        let failed_request =
            fetch_request_row_for_test(core.node(), &pre.failed_request_id).await?;
        assert_eq!(failed_request.lifecycle_state, case.post_failed_state);
        assert_eq!(failed_request.status, retry_parent_status_for_case(case));
        assert_eq!(failed_request.retry_count, case.pre_retry_count as i64);
        assert_eq!(failed_request.backend_id, case.pre_backend);
        assert_eq!(failed_request.execution_origin, case.pre_origin);
        assert_eq!(
            request_count_by_id_for_test(core.node(), &pre.failed_request_id).await?,
            if case.old_request_retained { 1 } else { 0 },
            "old failed request retention must match Lean witness for {}",
            case.name
        );
        assert_eq!(
            request_count_by_id_for_test(core.node(), new_request_id).await?,
            if case.new_request_inserted { 1 } else { 0 },
            "new request insertion must match Lean witness for {}",
            case.name
        );
        assert!(case.pre_failed_is_latest);
        assert!(!case.post_failed_is_latest);
        assert!(case.post_new_is_latest);

        Ok(())
    }

    async fn assert_illegal_session_recovery_post_state(
        core: &ClientCore,
        case: &LeanSessionRecoveryCase,
        pre: &RecoveryPreState,
        err: String,
        injected_new_request_id: Option<&str>,
    ) -> Result<()> {
        let expected = expected_illegal_guard_fragment(case);
        assert!(
            err.contains(expected),
            "illegal case {} should fail guard containing {expected:?}, got: {err}",
            case.name
        );
        assert_eq!(
            request_count_for_session_for_test(core.node(), &pre.session_id).await?,
            case.pre_request_count,
            "illegal case {} must not insert a retry request",
            case.name
        );
        assert_eq!(
            latest_request_id_for_session_for_test(core.node(), &pre.session_id).await?,
            pre.pre_latest_request_id,
            "illegal case {} must not change latest request",
            case.name
        );
        if let Some(request_id) = injected_new_request_id {
            assert_eq!(
                request_count_by_id_for_test(core.node(), request_id).await?,
                1,
                "duplicate-id guard for {} must not add another colliding row",
                case.name
            );
        }

        Ok(())
    }

    async fn retry_request_with_id_injection_for_test(
        core: &ClientCore,
        parent: &AgentRequestRow,
        injection: RetryRequestIdInjection,
    ) -> Result<SubmittedRequest> {
        let snapshot = core.store().snapshot();
        let submitted = retry_request_with_request_id(
            core.node(),
            snapshot.as_ref(),
            parent,
            injection.new_request_id,
        )
        .await?;
        core.store()
            .set_focused_request_id(Some(submitted.request_id.clone()));
        core.refresh_store().await?;
        Ok(submitted)
    }

    fn forced_retry_parent_state(case: &LeanSessionRecoveryCase) -> ForcedRequestState {
        ForcedRequestState {
            status: retry_parent_status_for_case(case),
            lifecycle_state: case.pre_failed_state.clone(),
            retry_count: case.pre_retry_count as i64,
            max_retries: case.max_retries as i64,
            deadline: recovery_deadline_for_case(case),
            backend_id: case.pre_backend.clone(),
            execution_origin: case.pre_origin.clone(),
        }
    }

    fn recovery_deadline_for_case(case: &LeanSessionRecoveryCase) -> String {
        let deadline = if case.pre_deadline_exceeded {
            chrono::Utc::now() - chrono::Duration::seconds(5)
        } else {
            chrono::Utc::now() + chrono::Duration::minutes(5)
        };
        deadline.to_rfc3339()
    }

    fn forced_latest_request_state(case: &LeanSessionRecoveryCase) -> ForcedRequestState {
        ForcedRequestState {
            status: status_for_lifecycle_state(&case.pre_latest_state),
            lifecycle_state: case.pre_latest_state.clone(),
            retry_count: 0,
            max_retries: case.max_retries as i64,
            deadline: (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
            backend_id: case.pre_backend.clone(),
            execution_origin: case.pre_origin.clone(),
        }
    }

    fn retry_parent_status_for_case(case: &LeanSessionRecoveryCase) -> &'static str {
        if case.pre_failed_state != "failed" {
            status_for_lifecycle_state(&case.pre_failed_state)
        } else if case.pre_failed_admission == "released" {
            "error"
        } else {
            "processing"
        }
    }

    fn status_for_lifecycle_state(lifecycle_state: &str) -> &'static str {
        match lifecycle_state {
            "failed" => "error",
            "pending" => "pending",
            "completed" => "completed",
            "superseded" => "superseded",
            "dead" => "dead",
            "interrupted" => "interrupted",
            _ => "processing",
        }
    }

    fn injected_new_request_id(
        case: &LeanSessionRecoveryCase,
        pre: &RecoveryPreState,
    ) -> Result<Option<RetryRequestIdInjection>> {
        if !case.pre_new_request_exists {
            return Ok(None);
        }

        let new_request_id = if case.new_id == case.failed_id {
            pre.failed_request_id.clone()
        } else {
            pre.existing_request_id.clone().with_context(|| {
                format!(
                    "Lean case {} needs an existing non-failed request id for new_id={}",
                    case.name, case.new_id
                )
            })?
        };

        Ok(Some(RetryRequestIdInjection { new_request_id }))
    }

    fn expected_illegal_guard_fragment(case: &LeanSessionRecoveryCase) -> &'static str {
        // Generated cases assert the first surfaced denial in this production
        // guard order, so future multi-violation cases should choose the same
        // precedence deliberately.
        if !case.pre_failed_exists {
            "not found"
        } else if case.pre_failed_state != "failed" || case.pre_failed_admission != "released" {
            "failed/error"
        } else if case.pre_retry_count >= case.max_retries {
            "exhausted retry budget"
        } else if case.pre_deadline_exceeded {
            "deadline is closed"
        } else if !case.pre_failed_is_latest {
            "must be latest"
        } else if case.pre_new_request_exists {
            "already exists"
        } else {
            panic!("unhandled illegal SessionRecovery case: {}", case.name);
        }
    }

    fn request_from_store_for_test(core: &ClientCore, request_id: &str) -> Result<AgentRequestRow> {
        core.store()
            .snapshot()
            .requests
            .iter()
            .find(|row| row.request_id == request_id)
            .cloned()
            .with_context(|| format!("expected request {request_id} in desktop store"))
    }

    async fn fetch_request_row_for_test(
        node: &EmbeddedNode,
        request_id: &str,
    ) -> Result<RecoveryRequestRow> {
        let escaped_request_id = escape_graphql_string(request_id);
        query_single_for_test(
            node,
            &format!(
                r#"{{
                    AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                        request_id
                        agent_did
                        behavior_id
                        session_id
                        content
                        temperature
                        top_p
                        top_k
                        max_tokens
                        metadata
                        status
                        lifecycle_state
                        backend_id
                        execution_origin
                        retry_root_request
                        retry_parent_request
                        retry_count
                        max_retries
                    }}
                }}"#
            ),
            "AgentRequest",
        )
        .await
    }

    async fn latest_request_id_for_session_for_test(
        node: &EmbeddedNode,
        session_id: &str,
    ) -> Result<String> {
        let escaped_session_id = escape_graphql_string(session_id);
        let conversation: RecoveryConversationRow = query_single_for_test(
            node,
            &format!(
                r#"{{
                    AgentConversation(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, limit: 1) {{
                        latest_request_id
                    }}
                }}"#
            ),
            "AgentConversation",
        )
        .await?;
        Ok(conversation.latest_request_id)
    }

    async fn request_count_for_session_for_test(
        node: &EmbeddedNode,
        session_id: &str,
    ) -> Result<usize> {
        let escaped_session_id = escape_graphql_string(session_id);
        query_row_count_for_test(
            node,
            &format!(
                r#"{{
                    AgentRequest(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}) {{
                        request_id
                    }}
                }}"#
            ),
            "AgentRequest",
        )
        .await
    }

    async fn request_count_by_id_for_test(node: &EmbeddedNode, request_id: &str) -> Result<usize> {
        let escaped_request_id = escape_graphql_string(request_id);
        query_row_count_for_test(
            node,
            &format!(
                r#"{{
                    AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}) {{
                        _docID
                    }}
                }}"#
            ),
            "AgentRequest",
        )
        .await
    }

    async fn query_single_for_test<T>(node: &EmbeddedNode, query: &str, root: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = node.execute(query).await;
        if response.has_errors() {
            bail!(
                "query {root} failed: {}",
                response
                    .errors
                    .iter()
                    .map(|error| error.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }

        let row = response
            .data
            .as_ref()
            .and_then(|data| data.get(root))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .cloned()
            .with_context(|| format!("missing row for {root}"))?;
        Ok(serde_json::from_value(row)?)
    }

    async fn query_row_count_for_test(
        node: &EmbeddedNode,
        query: &str,
        root: &str,
    ) -> Result<usize> {
        let response = node.execute(query).await;
        if response.has_errors() {
            bail!(
                "query {root} count failed: {}",
                response
                    .errors
                    .iter()
                    .map(|error| error.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }

        Ok(response
            .data
            .as_ref()
            .and_then(|data| data.get(root))
            .and_then(|rows| rows.as_array())
            .map(Vec::len)
            .unwrap_or_default())
    }

    async fn force_request_state_for_test(
        node: &EmbeddedNode,
        request_id: &str,
        state: &ForcedRequestState,
    ) -> Result<()> {
        let escaped_request_id = escape_graphql_string(request_id);
        let escaped_status = escape_graphql_string(state.status);
        let escaped_lifecycle_state = escape_graphql_string(&state.lifecycle_state);
        let escaped_deadline = escape_graphql_string(&state.deadline);
        let escaped_backend_id = escape_graphql_string(&state.backend_id);
        let escaped_execution_origin = escape_graphql_string(&state.execution_origin);
        let retry_count = state.retry_count;
        let max_retries = state.max_retries;
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    input: {{
                        status: "{escaped_status}",
                        lifecycle_state: "{escaped_lifecycle_state}",
                        retry_count: {retry_count},
                        max_retries: {max_retries},
                        deadline: "{escaped_deadline}",
                        backend_id: "{escaped_backend_id}",
                        execution_origin: "{escaped_execution_origin}"
                    }}
                ) {{ _docID }}
            }}"#
        );
        execute_mutation(node, &mutation, "force_request_state_for_test").await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn retry_request_with_injected_id_rejects_duplicate_new_request_id() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        )
        .await?;

        let created = core
            .create_conversation("did:defra:amy", Some("amy-code"))
            .await?;
        let original = core
            .submit_request(&created.session_id, "did:defra:amy", "first attempt", None)
            .await?;
        let mut parent = core
            .store()
            .snapshot()
            .requests
            .iter()
            .find(|row| row.request_id == original.request_id)
            .cloned()
            .context("expected submitted parent request in desktop store")?;

        let deadline = Utc::now() + chrono::Duration::minutes(5);
        force_retry_parent_eligible_for_test(
            core.node(),
            &original.request_id,
            1,
            i64::from(DEFAULT_REQUEST_MAX_RETRIES),
            &deadline.to_rfc3339(),
        )
        .await?;
        parent.status = Some("error".to_string());
        parent.lifecycle_state = Some("failed".to_string());
        parent.deadline = Some(deadline.to_rfc3339());
        parent.retry_count = Some(1);
        parent.max_retries = Some(i64::from(DEFAULT_REQUEST_MAX_RETRIES));

        let duplicate_request_id = "duplicate-retry-request-id";
        seed_duplicate_request_id_for_test(
            core.node(),
            duplicate_request_id,
            &created.session_id,
            "did:defra:amy",
            "amy-code",
        )
        .await?;
        assert_eq!(
            request_count_by_id_for_test(core.node(), duplicate_request_id).await?,
            1
        );

        let snapshot = core.store().snapshot();
        let err = retry_request_with_request_id(
            core.node(),
            snapshot.as_ref(),
            &parent,
            duplicate_request_id.to_string(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("already exists"),
            "duplicate new request id must be rejected before retry insert: {err}"
        );
        assert_eq!(
            request_count_by_id_for_test(core.node(), duplicate_request_id).await?,
            1,
            "failed duplicate retry must not add another row with the colliding request_id"
        );

        core.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn retry_request_preserves_parent_overrides_and_metadata() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        )
        .await?;

        let created = core
            .create_conversation("did:defra:amy", Some("amy-code"))
            .await?;
        let metadata = r#"{"eval":"amygdala","case":"retry"}"#.to_string();
        let original = core
            .submit_request_with_options(
                &created.session_id,
                "did:defra:amy",
                "retry should preserve overrides",
                None,
                SubmitRequestOptions {
                    temperature: Some(0.35),
                    top_p: Some(0.92),
                    top_k: Some(32),
                    max_tokens: Some(2048),
                    metadata: Some(metadata.clone()),
                    ..SubmitRequestOptions::default()
                },
            )
            .await?;
        let deadline = Utc::now() + chrono::Duration::minutes(5);
        force_retry_parent_eligible_for_test(
            core.node(),
            &original.request_id,
            1,
            i64::from(DEFAULT_REQUEST_MAX_RETRIES),
            &deadline.to_rfc3339(),
        )
        .await?;
        core.refresh_store().await?;

        let parent = request_from_store_for_test(&core, &original.request_id)?;
        assert_eq!(parent.temperature, Some(0.35));
        assert_eq!(parent.top_p, Some(0.92));
        assert_eq!(parent.top_k, Some(32));
        assert_eq!(parent.max_tokens, Some(2048));
        assert_eq!(parent.metadata.as_deref(), Some(metadata.as_str()));

        let submitted = core.retry_request(&parent).await?;
        let retried = fetch_request_row_for_test(core.node(), &submitted.request_id).await?;
        assert_eq!(retried.retry_parent_request, original.request_id);
        assert_eq!(retried.retry_root_request, original.request_id);
        assert_eq!(retried.temperature, Some(0.35));
        assert_eq!(retried.top_p, Some(0.92));
        assert_eq!(retried.top_k, Some(32));
        assert_eq!(retried.max_tokens, Some(2048));
        assert_eq!(retried.metadata.as_deref(), Some(metadata.as_str()));

        core.shutdown().await?;
        Ok(())
    }

    async fn force_retry_parent_eligible_for_test(
        node: &EmbeddedNode,
        request_id: &str,
        retry_count: i64,
        max_retries: i64,
        deadline: &str,
    ) -> Result<()> {
        let escaped_request_id = escape_graphql_string(request_id);
        let escaped_deadline = escape_graphql_string(deadline);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    input: {{
                        status: "error",
                        lifecycle_state: "failed",
                        retry_count: {retry_count},
                        max_retries: {max_retries},
                        deadline: "{escaped_deadline}"
                    }}
                ) {{ _docID }}
            }}"#
        );
        execute_mutation(node, &mutation, "force_retry_parent_eligible_for_test").await
    }

    async fn seed_duplicate_request_id_for_test(
        node: &EmbeddedNode,
        request_id: &str,
        session_id: &str,
        agent_did: &str,
        behavior_id: &str,
    ) -> Result<()> {
        let created_at = Utc::now().to_rfc3339();
        let request_field = build_add_agent_request_field(
            "duplicate",
            request_id,
            agent_did,
            behavior_id,
            session_id,
            "",
            "",
            "existing duplicate request id occupant",
            &created_at,
            0,
            i64::from(DEFAULT_REQUEST_MAX_RETRIES),
            "",
            "interactive",
            "",
        );
        let mutation = format!("mutation {{\n{request_field}\n}}");
        execute_mutation(node, &mutation, "seed_duplicate_request_id_for_test").await
    }
}
