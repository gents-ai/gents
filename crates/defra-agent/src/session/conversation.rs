use super::query::load_conversation_document;
use super::retry::execute_mutation_with_retry;
use super::rows::ConversationDocument;
use super::*;

#[allow(dead_code)]
pub(crate) async fn upsert_conversation_from_request(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    request_id: &str,
    content: &str,
    status: &str,
) -> Result<()> {
    upsert_conversation_from_request_with_identity(
        node,
        session_id,
        agent_name,
        &agent_did_for_name(agent_name),
        agent_name,
        request_id,
        content,
        status,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert_conversation_from_request_with_identity(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    agent_did: &str,
    behavior_id: &str,
    request_id: &str,
    content: &str,
    status: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_preview = escape_graphql_string(&derive_conversation_preview(content));
    let escaped_status = escape_graphql_string(status);
    let existing = load_conversation_document(node, session_id).await?;
    let resolved_behavior_id =
        resolve_behavior_id(existing.as_ref(), behavior_id, "AgentConversation")?;
    let title = if let Some(existing) = existing.as_ref() {
        if existing.title.trim().is_empty() || existing.title == "New Conversation" {
            derive_conversation_title(content)
        } else {
            existing.title.clone()
        }
    } else {
        derive_conversation_title(content)
    };
    let created_at = existing
        .as_ref()
        .map(|conversation| conversation.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let escaped_behavior_id = escape_graphql_string(&resolved_behavior_id);
    let escaped_title = escape_graphql_string(&title);
    let escaped_created_at = escape_graphql_string(&created_at);

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
                    latest_request_id: "{escaped_request_id}"
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
                    latest_request_id: "{escaped_request_id}"
                }}
            ) {{ _docID }}
        }}"#
    );

    execute_mutation_with_retry(node, &mutation, "upsert_conversation_from_request").await?;
    Ok(())
}

pub(crate) async fn update_conversation_status(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    status: &str,
) -> Result<()> {
    update_conversation_status_with_identity(
        node,
        session_id,
        agent_name,
        &agent_did_for_name(agent_name),
        agent_name,
        status,
    )
    .await
}

pub(crate) async fn update_conversation_status_with_identity(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    agent_did: &str,
    behavior_id: &str,
    status: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_status = escape_graphql_string(status);
    let existing = load_conversation_document(node, session_id).await?;
    let resolved_behavior_id =
        resolve_behavior_id(existing.as_ref(), behavior_id, "AgentConversation")?;
    let title = existing
        .as_ref()
        .map(|conversation| conversation.title.clone())
        .unwrap_or_else(|| "New Conversation".to_string());
    let preview_text = existing
        .as_ref()
        .map(|conversation| conversation.preview_text.clone())
        .unwrap_or_default();
    let latest_request_id = existing
        .as_ref()
        .map(|conversation| conversation.latest_request_id.clone())
        .unwrap_or_default();
    let created_at = existing
        .as_ref()
        .map(|conversation| conversation.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let escaped_behavior_id = escape_graphql_string(&resolved_behavior_id);
    let escaped_title = escape_graphql_string(&title);
    let escaped_preview_text = escape_graphql_string(&preview_text);
    let escaped_latest_request_id = escape_graphql_string(&latest_request_id);
    let escaped_created_at = escape_graphql_string(&created_at);

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
                    preview_text: "{escaped_preview_text}",
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
                    preview_text: "{escaped_preview_text}",
                    status: "{escaped_status}",
                    created_at: "{escaped_created_at}",
                    updated_at: "{now}",
                    latest_request_id: "{escaped_latest_request_id}"
                }}
            ) {{ _docID }}
        }}"#
    );

    execute_mutation_with_retry(node, &mutation, "update_conversation_status").await?;
    Ok(())
}

pub(crate) async fn update_conversation_status_if_latest_with_identity(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    agent_did: &str,
    behavior_id: &str,
    latest_request_id: &str,
    status: &str,
) -> Result<ConversationUpdateOutcome> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_latest_request_id = escape_graphql_string(latest_request_id);
    let escaped_status = escape_graphql_string(status);
    let existing = load_conversation_document(node, session_id).await?;

    let Some(existing) = existing else {
        return Ok(ConversationUpdateOutcome::SkippedStaleRequest);
    };
    let resolved_behavior_id =
        resolve_behavior_id(Some(&existing), behavior_id, "AgentConversation")?;

    if existing.latest_request_id != latest_request_id {
        return Ok(ConversationUpdateOutcome::SkippedStaleRequest);
    }

    if existing.status == status {
        return Ok(ConversationUpdateOutcome::AlreadyApplied);
    }

    let escaped_title = escape_graphql_string(&existing.title);
    let escaped_preview_text = escape_graphql_string(&existing.preview_text);
    let escaped_created_at = escape_graphql_string(&existing.created_at);
    let escaped_behavior_id = escape_graphql_string(&resolved_behavior_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentConversation(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    latest_request_id: {{ _eq: "{escaped_latest_request_id}" }}
                }},
                input: {{
                    agent_name: "{escaped_agent_name}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    title: "{escaped_title}",
                    preview_text: "{escaped_preview_text}",
                    status: "{escaped_status}",
                    created_at: "{escaped_created_at}",
                    updated_at: "{now}",
                    latest_request_id: "{escaped_latest_request_id}"
                }}
            ) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!(
            "update_conversation_status_if_latest mutation failed for session_id={session_id}: {:?}",
            resp.errors
        );
    }

    if resp
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentConversation"))
        .is_some_and(response_has_documents)
    {
        return Ok(ConversationUpdateOutcome::Updated);
    }

    match load_conversation_document(node, session_id).await? {
        Some(latest) if latest.latest_request_id != latest_request_id => {
            Ok(ConversationUpdateOutcome::SkippedStaleRequest)
        }
        Some(latest) if latest.status == status => Ok(ConversationUpdateOutcome::AlreadyApplied),
        Some(latest) => anyhow::bail!(
            "conversation session_id={session_id} stayed at status={} for latest_request_id={}",
            latest.status,
            latest.latest_request_id
        ),
        None => anyhow::bail!(
            "conversation disappeared while updating session_id={session_id} latest_request_id={latest_request_id}"
        ),
    }
}

fn agent_did_for_name(agent_name: &str) -> String {
    format!("did:defra-agent:{agent_name}")
}

fn resolve_behavior_id(
    existing: Option<&ConversationDocument>,
    requested_behavior_id: &str,
    collection_name: &str,
) -> Result<String> {
    let existing_behavior_id = existing
        .and_then(|conversation| normalize_optional_string(conversation.behavior_id.as_deref()));
    let requested_behavior_id = normalize_optional_string(Some(requested_behavior_id));

    match (existing_behavior_id, requested_behavior_id) {
        (Some(existing), Some(requested)) if existing != requested => anyhow::bail!(
            "{collection_name} session behavior mismatch: existing={existing} requested={requested}"
        ),
        (Some(existing), _) => Ok::<String, anyhow::Error>(existing.to_string()),
        (None, Some(requested)) => Ok::<String, anyhow::Error>(requested.to_string()),
        (None, None) => Ok(String::new()),
    }
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
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
