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
    let parent_request_id = normalize_required("request_id", &parent.request_id)?;
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
    let request_id = Uuid::new_v4().to_string();
    let session_id = Uuid::new_v4().to_string();
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
        "",
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
                backend_id: "",
                execution_origin: "interactive",
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
