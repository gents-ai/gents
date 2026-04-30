use super::query::load_conversation_document;
use super::query::load_recent_conversation_titles;
use super::retry::execute_mutation_with_retry;
use super::rows::ConversationDocument;
use super::*;

pub(crate) const CONVERSATION_TITLE_SOURCE_PLACEHOLDER: &str = "placeholder";
pub(crate) const CONVERSATION_TITLE_SOURCE_GENERATED: &str = "generated";
pub(crate) const CONVERSATION_TITLE_SOURCE_TASK: &str = "task";

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
    upsert_conversation_from_request_with_identity_and_title(
        node,
        session_id,
        agent_name,
        agent_did,
        behavior_id,
        request_id,
        content,
        status,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert_conversation_from_request_with_identity_and_title(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    agent_did: &str,
    behavior_id: &str,
    request_id: &str,
    content: &str,
    status: &str,
    title_override: Option<(&str, &str)>,
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
    let (title, title_source) = existing_title_state(existing.as_ref(), title_override);
    let created_at = existing
        .as_ref()
        .map(|conversation| conversation.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let escaped_behavior_id = escape_graphql_string(&resolved_behavior_id);
    let escaped_title = escape_graphql_string(&title);
    let escaped_title_source = escape_graphql_string(&title_source);
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
                    title_source: "{escaped_title_source}",
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
                    title_source: "{escaped_title_source}",
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
        .unwrap_or_default();
    let title_source = existing
        .as_ref()
        .and_then(|conversation| normalize_optional_string(conversation.title_source.as_deref()))
        .unwrap_or(CONVERSATION_TITLE_SOURCE_PLACEHOLDER);
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
    let escaped_title_source = escape_graphql_string(title_source);
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
                    title_source: "{escaped_title_source}",
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
                    title_source: "{escaped_title_source}",
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
    let escaped_title_source = escape_graphql_string(
        existing
            .title_source
            .as_deref()
            .unwrap_or(CONVERSATION_TITLE_SOURCE_PLACEHOLDER),
    );
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
                    title_source: "{escaped_title_source}",
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

pub(crate) async fn update_conversation_title_with_source(
    node: &EmbeddedNode,
    session_id: &str,
    title: &str,
    title_source: &str,
) -> Result<()> {
    let Some(existing) = load_conversation_document(node, session_id).await? else {
        return Ok(());
    };

    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_title = escape_graphql_string(title);
    let escaped_title_source = escape_graphql_string(title_source);
    let escaped_preview_text = escape_graphql_string(&existing.preview_text);
    let escaped_status = escape_graphql_string(&existing.status);
    let escaped_latest_request_id = escape_graphql_string(&existing.latest_request_id);
    let escaped_created_at = escape_graphql_string(&existing.created_at);
    let escaped_behavior_id =
        escape_graphql_string(existing.behavior_id.as_deref().unwrap_or_default());

    let mutation = format!(
        r#"mutation {{
            update_AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                input: {{
                    behavior_id: "{escaped_behavior_id}",
                    title: "{escaped_title}",
                    title_source: "{escaped_title_source}",
                    preview_text: "{escaped_preview_text}",
                    status: "{escaped_status}",
                    created_at: "{escaped_created_at}",
                    updated_at: "{now}",
                    latest_request_id: "{escaped_latest_request_id}"
                }}
            ) {{ _docID }}
        }}"#
    );

    execute_mutation_with_retry(node, &mutation, "update_conversation_title_with_source").await?;
    Ok(())
}

pub(crate) async fn load_recent_titles_for_agent(
    node: &EmbeddedNode,
    agent_did: &str,
    exclude_session_id: &str,
    limit: usize,
) -> Result<Vec<String>> {
    load_recent_conversation_titles(node, agent_did, exclude_session_id, limit).await
}

pub(crate) async fn conversation_needs_generated_title(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<bool> {
    let Some(existing) = load_conversation_document(node, session_id).await? else {
        return Ok(false);
    };

    let title = existing.title.trim();
    let title_source = normalize_optional_string(existing.title_source.as_deref())
        .unwrap_or(CONVERSATION_TITLE_SOURCE_PLACEHOLDER);

    Ok(title.is_empty() || title_source == CONVERSATION_TITLE_SOURCE_PLACEHOLDER)
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

fn existing_title_state(
    existing: Option<&ConversationDocument>,
    title_override: Option<(&str, &str)>,
) -> (String, String) {
    let normalized_override = title_override.and_then(|(title, source)| {
        let title = title.trim();
        let source = source.trim();
        (!title.is_empty() && !source.is_empty()).then(|| (title.to_string(), source.to_string()))
    });

    match existing {
        Some(existing) => {
            let existing_title = existing.title.trim();
            let existing_source = normalize_optional_string(existing.title_source.as_deref())
                .unwrap_or(CONVERSATION_TITLE_SOURCE_PLACEHOLDER);
            if existing_title.is_empty() || existing_source == CONVERSATION_TITLE_SOURCE_PLACEHOLDER
            {
                if let Some((title, source)) = normalized_override.as_ref() {
                    return (title.clone(), source.clone());
                }
            }

            (existing.title.clone(), existing_source.to_string())
        }
        None => normalized_override.unwrap_or_else(|| {
            (
                String::new(),
                CONVERSATION_TITLE_SOURCE_PLACEHOLDER.to_string(),
            )
        }),
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
