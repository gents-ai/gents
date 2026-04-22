use anyhow::Result;
use chrono::Utc;
use defra_node::EmbeddedNode;
use uuid::Uuid;

use crate::client::store::ClientStore;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, normalize_optional_string, normalize_required,
};
use super::binding::resolve_agent_binding;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedConversation {
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

pub async fn rename_conversation(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    title: &str,
) -> Result<()> {
    let session_id = normalize_required("session_id", session_id)?;
    let title = normalize_required("title", title)?;
    let existing = store
        .conversations
        .iter()
        .find(|row| row.session_id == session_id)
        .ok_or_else(|| anyhow::anyhow!("conversation {} not found", session_id))?;

    let now = Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_behavior_id =
        escape_graphql_string(existing.behavior_id.as_deref().unwrap_or_default());
    let escaped_title = escape_graphql_string(title);
    let escaped_preview =
        escape_graphql_string(existing.preview_text.as_deref().unwrap_or_default());
    let escaped_status = escape_graphql_string(existing.status.as_deref().unwrap_or("active"));
    let escaped_created_at = escape_graphql_string(
        normalize_optional_string(existing.created_at.as_deref()).unwrap_or(now.as_str()),
    );
    let escaped_latest_request_id =
        escape_graphql_string(existing.latest_request_id.as_deref().unwrap_or_default());
    let mutation = format!(
        r#"mutation {{
            update_AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                input: {{
                    behavior_id: "{escaped_behavior_id}",
                    title: "{escaped_title}",
                    title_source: "user",
                    preview_text: "{escaped_preview}",
                    status: "{escaped_status}",
                    created_at: "{escaped_created_at}",
                    updated_at: "{now}",
                    latest_request_id: "{escaped_latest_request_id}"
                }}
            ) {{ _docID }}
        }}"#
    );
    execute_mutation(node, &mutation, "rename_conversation").await
}

pub(super) async fn upsert_session(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
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
pub(super) async fn upsert_conversation(
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
        .map(ToOwned::to_owned)
        .unwrap_or_default();
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

fn derive_conversation_preview(content: &str) -> String {
    truncate_chars(&normalize_conversation_text(content), 240)
}

fn normalize_conversation_text(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
