use anyhow::{bail, Context, Result};
use chrono::Utc;
use defra_agent_protocol::row::AgentRequestRow;
use defra_node::EmbeddedNode;
use uuid::Uuid;

use crate::client::store::ClientStore;

use super::graphql::{
    escape_graphql_string, execute_mutation, normalize_optional_string, normalize_required,
};

const DEFAULT_REQUEST_MAX_RETRIES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedConversation {
    pub session_id: String,
    pub agent_did: String,
    pub behavior_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedRequest {
    pub request_id: String,
    pub session_id: String,
    pub agent_did: String,
    pub behavior_id: Option<String>,
}

pub async fn create_conversation(
    node: &EmbeddedNode,
    store: &ClientStore,
    agent_did: &str,
    behavior_id: Option<&str>,
) -> Result<CreatedConversation> {
    let agent_did = normalize_required("agent_did", agent_did)?;
    let session_id = Uuid::new_v4().to_string();
    let binding = resolve_agent_binding(store, agent_did, behavior_id, None)?;

    upsert_session(
        node,
        store,
        &session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
    )
    .await?;
    upsert_conversation(
        node,
        store,
        &session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
        "",
        "",
        "active",
    )
    .await?;

    Ok(CreatedConversation {
        session_id,
        agent_did: agent_did.to_string(),
        behavior_id: binding.behavior_id,
    })
}

pub async fn submit_request(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
    content: &str,
    behavior_id: Option<&str>,
) -> Result<SubmittedRequest> {
    let session_id = normalize_required("session_id", session_id)?;
    let agent_did = normalize_required("agent_did", agent_did)?;
    let content = normalize_required("content", content)?;
    let request_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let binding = resolve_agent_binding(store, agent_did, behavior_id, Some(session_id))?;

    upsert_session(
        node,
        store,
        session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
    )
    .await?;

    let escaped_request_id = escape_graphql_string(&request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(binding.behavior_id.as_deref().unwrap_or(""));
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_content = escape_graphql_string(content);
    let escaped_created_at = escape_graphql_string(&created_at);

    let mutation = format!(
        r#"mutation {{
            add_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "{escaped_content}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
    );
    execute_mutation(node, &mutation, "submit_request").await?;

    upsert_conversation(
        node,
        store,
        session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
        &request_id,
        content,
        "active",
    )
    .await?;

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
    let session_id = normalize_required(
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
    let created_at = Utc::now().to_rfc3339();
    let binding = resolve_agent_binding(store, agent_did, behavior_id, Some(session_id))?;

    upsert_session(
        node,
        store,
        session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
    )
    .await?;

    let escaped_request_id = escape_graphql_string(&request_id);
    let escaped_parent_request_id = escape_graphql_string(parent_request_id);
    let escaped_retry_root_request = escape_graphql_string(retry_root_request);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(binding.behavior_id.as_deref().unwrap_or(""));
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_content = escape_graphql_string(content);
    let escaped_created_at = escape_graphql_string(&created_at);

    let mutation = format!(
        r#"mutation {{
            add_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "{escaped_parent_request_id}",
                retry_root_request: "{escaped_retry_root_request}",
                superseded_by_request: "",
                content: "{escaped_content}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: {retry_count},
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#
    );
    execute_mutation(node, &mutation, "retry_request").await?;

    upsert_conversation(
        node,
        store,
        session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
        &request_id,
        content,
        "active",
    )
    .await?;

    Ok(SubmittedRequest {
        request_id,
        session_id: session_id.to_string(),
        agent_did: agent_did.to_string(),
        behavior_id: binding.behavior_id,
    })
}

struct ResolvedAgentBinding {
    agent_name: String,
    behavior_id: Option<String>,
}

fn resolve_agent_binding(
    store: &ClientStore,
    agent_did: &str,
    requested_behavior_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<ResolvedAgentBinding> {
    let existing_conversation = session_id.and_then(|session_id| {
        store
            .conversations
            .iter()
            .find(|row| row.session_id == session_id)
    });
    let existing_session = session_id.and_then(|session_id| {
        store
            .sessions
            .iter()
            .find(|row| row.session_id == session_id)
    });

    let behavior_id = resolve_behavior_id(
        store,
        agent_did,
        requested_behavior_id,
        existing_conversation.and_then(|row| row.behavior_id.as_deref()),
        existing_session.and_then(|row| row.behavior_id.as_deref()),
    )?;
    let agent_name = existing_conversation
        .and_then(|row| normalize_optional_string(row.agent_name.as_deref()))
        .or_else(|| {
            existing_session.and_then(|row| normalize_optional_string(row.agent_name.as_deref()))
        })
        .or_else(|| {
            store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == agent_did)
                .and_then(|row| normalize_optional_string(row.display_name.as_deref()))
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_display_name_for_did(agent_did));

    Ok(ResolvedAgentBinding {
        agent_name,
        behavior_id,
    })
}

fn resolve_behavior_id(
    store: &ClientStore,
    agent_did: &str,
    requested_behavior_id: Option<&str>,
    existing_conversation_behavior_id: Option<&str>,
    existing_session_behavior_id: Option<&str>,
) -> Result<Option<String>> {
    let requested = normalize_optional_string(requested_behavior_id);

    let conversation_behavior = normalize_optional_string(existing_conversation_behavior_id);
    let session_behavior = normalize_optional_string(existing_session_behavior_id);

    if let (Some(existing), Some(requested)) = (conversation_behavior, requested) {
        if existing != requested {
            bail!(
                "AgentConversation session behavior mismatch: existing={existing} requested={requested}"
            );
        }
    }

    if let (Some(existing), Some(requested)) = (session_behavior, requested) {
        if existing != requested {
            bail!(
                "AgentSession session behavior mismatch: existing={existing} requested={requested}"
            );
        }
    }

    let resolved = conversation_behavior
        .or(session_behavior)
        .or(requested)
        .or_else(|| {
            store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == agent_did)
                .and_then(|row| normalize_optional_string(row.default_behavior_id.as_deref()))
        })
        .or_else(|| {
            store
                .behaviors
                .iter()
                .find(|row| {
                    row.agent_did.as_deref() == Some(agent_did) && row.enabled != Some(false)
                })
                .map(|row| row.behavior_id.as_str())
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_behavior_id_for_agent(agent_did));

    Ok(normalize_optional_string(Some(&resolved)).map(ToOwned::to_owned))
}

async fn upsert_session(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    _agent_did: &str,
    agent_name: &str,
    behavior_id: &Option<String>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let existing = store
        .sessions
        .iter()
        .find(|row| row.session_id == session_id);
    let started = existing
        .and_then(|row| normalize_optional_string(row.started.as_deref()))
        .unwrap_or(now.as_str());

    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);
    let escaped_behavior_id = escape_graphql_string(behavior_id.as_deref().unwrap_or(""));
    let escaped_started = escape_graphql_string(started);
    let mutation = format!(
        r#"mutation {{
            upsert_AgentSession(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_agent_name}",
                    behavior_id: "{escaped_behavior_id}",
                    started: "{escaped_started}",
                    status: "active"
                }},
                update: {{
                    agent_name: "{escaped_agent_name}",
                    behavior_id: "{escaped_behavior_id}",
                    started: "{escaped_started}",
                    status: "active"
                }}
            ) {{ _docID }}
        }}"#
    );
    execute_mutation(node, &mutation, "upsert_session").await
}

#[allow(clippy::too_many_arguments)]
async fn upsert_conversation(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
    agent_name: &str,
    behavior_id: &Option<String>,
    latest_request_id: &str,
    content: &str,
    status: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let existing = store
        .conversations
        .iter()
        .find(|row| row.session_id == session_id);

    let title = existing
        .and_then(|row| normalize_optional_string(row.title.as_deref()))
        .filter(|title| *title != "New Conversation")
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| derive_conversation_title(content));
    let preview_text = if content.is_empty() {
        existing
            .and_then(|row| row.preview_text.as_deref())
            .unwrap_or_default()
            .to_string()
    } else {
        derive_conversation_preview(content)
    };
    let created_at = existing
        .and_then(|row| normalize_optional_string(row.created_at.as_deref()))
        .unwrap_or(now.as_str());
    let latest_request_id = normalize_optional_string(Some(latest_request_id))
        .or_else(|| {
            existing.and_then(|row| normalize_optional_string(row.latest_request_id.as_deref()))
        })
        .unwrap_or_default();

    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(behavior_id.as_deref().unwrap_or(""));
    let escaped_title = escape_graphql_string(&title);
    let escaped_preview = escape_graphql_string(&preview_text);
    let escaped_status = escape_graphql_string(status);
    let escaped_created_at = escape_graphql_string(created_at);
    let escaped_latest_request_id = escape_graphql_string(latest_request_id);
    let mutation = format!(
        r#"mutation {{
            upsert_AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_agent_name}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    title: "{escaped_title}",
                    preview_text: "{escaped_preview}",
                    status: "{escaped_status}",
                    created_at: "{escaped_created_at}",
                    updated_at: "{now}",
                    latest_request_id: "{escaped_latest_request_id}"
                }},
                update: {{
                    agent_name: "{escaped_agent_name}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    title: "{escaped_title}",
                    preview_text: "{escaped_preview}",
                    status: "{escaped_status}",
                    created_at: "{escaped_created_at}",
                    updated_at: "{now}",
                    latest_request_id: "{escaped_latest_request_id}"
                }}
            ) {{ _docID }}
        }}"#
    );
    execute_mutation(node, &mutation, "upsert_conversation").await
}

fn default_behavior_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:default")
}

fn default_display_name_for_did(agent_did: &str) -> String {
    agent_did
        .rsplit(':')
        .next()
        .filter(|segment| !segment.trim().is_empty())
        .unwrap_or(agent_did)
        .to_string()
}

fn derive_conversation_title(content: &str) -> String {
    let normalized = normalize_conversation_text(content);
    if normalized.is_empty() {
        "New Conversation".to_string()
    } else {
        truncate_chars(&normalized, 80)
    }
}

fn derive_conversation_preview(content: &str) -> String {
    truncate_chars(&normalize_conversation_text(content), 240)
}

fn normalize_conversation_text(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_behavior_id_uses_agent_did_suffix() {
        assert_eq!(
            default_behavior_id_for_agent("did:defra:test"),
            "did:defra:test:default".to_string()
        );
    }

    #[test]
    fn conversation_title_defaults_for_empty_content() {
        assert_eq!(derive_conversation_title(""), "New Conversation");
    }
}
