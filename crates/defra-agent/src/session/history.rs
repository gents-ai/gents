use super::retry::execute_query_timed;
use super::rows::AgentMessageRow;
use super::*;
use defra_agent_protocol::transcript::decode_persisted_message;

pub async fn load_history(node: &EmbeddedNode, session_id: &str) -> Result<Vec<Message>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                sequence
                role
                content
                timestamp
            }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "load_history").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading history for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    let messages: Vec<AgentMessageRow> =
        match resp.data.as_ref().and_then(|data| data.get("AgentMessage")) {
            Some(value) => serde_json::from_value(value.clone())?,
            None => Vec::new(),
        };

    let mut history = Vec::with_capacity(messages.len());
    for msg in messages {
        history.push(decode_persisted_message(
            msg.role.as_str(),
            msg.content.as_str(),
        ));
    }

    tracing::debug!(session_id = %session_id, count = history.len(), "loaded history");
    Ok(history)
}

pub(crate) async fn save_message(
    node: &EmbeddedNode,
    session_id: &str,
    sequence: u32,
    role: &str,
    content: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped = escape_graphql_string(content);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_role = escape_graphql_string(role);
    let message_key = format!("{escaped_session_id}:{sequence}");

    let mutation = format!(
        r#"mutation {{
            upsert_AgentMessage(
                filter: {{ message_key: {{ _eq: "{message_key}" }} }},
                add: {{
                    message_key: "{message_key}",
                    session_id: "{escaped_session_id}",
                    sequence: {sequence},
                    role: "{escaped_role}",
                    content: "{escaped}",
                    timestamp: "{now}"
                }},
                update: {{
                    content: "{escaped}",
                    timestamp: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );

    super::retry::execute_mutation_with_retry(node, &mutation, "save_message").await?;
    Ok(())
}
