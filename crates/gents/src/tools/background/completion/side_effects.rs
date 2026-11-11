use super::*;

pub(super) struct ExistingNotification {
    pub(super) doc_id: String,
}

async fn existing_notification(
    access: &crate::ConfigAccess,
    parent: &gents_protocol::row::AgentRequest,
    message_key: &str,
) -> Result<Option<ExistingNotification>> {
    use gents_protocol::document_query::{
        fields::agent_message as message, session_scope, Filter, Query, Selection,
    };
    let query = Query::select(Selection::doc_id())
        .filter(session_scope(
            &parent.node_did,
            &parent.session_id,
            parent.requester_did.as_deref(),
        ))
        .filter(Filter::eq(message::MESSAGE_KEY, message_key))
        .limit(2)
        .render();
    let response = access.read(&query).await?;
    let rows: Vec<serde_json::Value> =
        serde_json::from_value(response["data"]["AgentMessage"].clone())?;
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
    access: &crate::ConfigAccess,
    parent: &gents_protocol::row::AgentRequest,
    tool_call_id: &str,
) -> Result<Option<ExistingNotification>> {
    let key = background_completion_notification_message_key(tool_call_id, "tool");
    existing_notification(access, parent, &key).await
}
