use super::*;

pub(super) struct ExistingNotification {
    pub(super) doc_id: String,
}

async fn existing_notification(
    node: &EmbeddedNode,
    parent: &crate::AgentRequest,
    message_key: &str,
) -> Result<Option<ExistingNotification>> {
    let scope = crate::session::session_scope_filter(
        &parent.node_did,
        &parent.session_id,
        parent.requester_did.as_deref(),
    );
    let message_key = escape_graphql_string(message_key);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ {scope}, message_key: {{ _eq: "{message_key}" }} }},
                limit: 2
            ) {{
                _docID
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent canonical notification key {message_key} failed: {:?}",
            response.errors
        );
    }
    let rows = crate::graphql::rows::<serde_json::Value>(&response, "AgentMessage")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "canonical notification key {message_key} is ambiguous"
    );
    match rows.as_slice() {
        [] => Ok(None),
        [row] => {
            let doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .filter(|doc_id| !doc_id.is_empty())
                .context("canonical notification key matched a row without _docID")?;
            Ok(Some(ExistingNotification {
                doc_id: doc_id.to_owned(),
            }))
        }
        _ => unreachable!("duplicate canonical notification rows were rejected above"),
    }
}

pub(super) async fn existing_tool_completion_notification(
    node: &EmbeddedNode,
    parent: &crate::AgentRequest,
    tool_call_id: &str,
) -> Result<Option<ExistingNotification>> {
    let key = background_completion_notification_message_key(tool_call_id, "tool");
    existing_notification(node, parent, &key).await
}
