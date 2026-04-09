use super::rows::{ConversationDocument, SessionDocument};
use super::*;

pub(super) async fn load_session_document(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<SessionDocument> {
    load_session_document_optional(node, session_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "loading session for completion: no AgentSession for session_id={session_id}"
            )
        })
}

pub(super) async fn load_session_document_optional(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Option<SessionDocument>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }}
                }},
                limit: 1
            ) {{
                _docID
                behavior_id
                started
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading session for completion session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    let mut rows: Vec<SessionDocument> =
        match resp.data.as_ref().and_then(|data| data.get("AgentSession")) {
            Some(value) => serde_json::from_value(value.clone())?,
            None => Vec::new(),
        };

    Ok(rows.pop())
}

pub(crate) async fn load_session_behavior_id(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Option<String>> {
    Ok(load_session_document_optional(node, session_id)
        .await?
        .and_then(|session| {
            session.behavior_id.and_then(|behavior_id| {
                let trimmed = behavior_id.trim().to_string();
                (!trimmed.is_empty()).then_some(trimmed)
            })
        }))
}

pub(super) async fn load_conversation_document(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Option<ConversationDocument>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }}
                }},
                limit: 1
            ) {{
                _docID
                title
                preview_text
                status
                latest_request_id
                behavior_id
                created_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading conversation document for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    let mut rows: Vec<ConversationDocument> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentConversation"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    Ok(rows.pop())
}
