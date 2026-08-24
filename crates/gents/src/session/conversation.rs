use super::query::load_conversation_document;
use super::query::load_recent_conversation_titles;
use super::retry::execute_mutation_with_retry;
use super::*;

pub(crate) const CONVERSATION_TITLE_SOURCE_FALLBACK: &str = "placeholder";
pub(crate) const CONVERSATION_TITLE_SOURCE_GENERATED: &str = "generated";
pub(crate) const CONVERSATION_TITLE_SOURCE_TASK: &str = "task";

pub(crate) fn request_conversation_status_projection_mutation(
    session_id: &str,
    latest_request_id: &str,
    status: &str,
    updated_at: &str,
) -> String {
    let session_id = escape_graphql_string(session_id);
    let latest_request_id = escape_graphql_string(latest_request_id);
    let status = escape_graphql_string(status);
    let updated_at = escape_graphql_string(updated_at);
    format!(
        r#"mutation {{
            update_AgentConversation(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    latest_request_id: {{ _eq: "{latest_request_id}" }}
                }},
                input: {{
                    status: "{status}",
                    updated_at: "{updated_at}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

pub(crate) async fn update_conversation_title_with_source(
    node: &EmbeddedNode,
    session_id: &str,
    title: &str,
    title_source: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_title = escape_graphql_string(title);
    let escaped_title_source = escape_graphql_string(title_source);
    let mutation = format!(
        r#"mutation {{
            update_AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                input: {{
                    title: "{escaped_title}",
                    title_source: "{escaped_title_source}",
                    updated_at: "{now}"
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
        .unwrap_or(CONVERSATION_TITLE_SOURCE_FALLBACK);

    Ok(title.is_empty() || title_source == CONVERSATION_TITLE_SOURCE_FALLBACK)
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(crate) fn derive_conversation_preview(content: &str) -> String {
    truncate_chars(&normalize_conversation_text(content), 240)
}

fn normalize_conversation_text(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
