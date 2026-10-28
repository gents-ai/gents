use super::{
    fake_llm::{self, ChatAction},
    read_session,
    support::Attachment,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

pub fn reply(request: &Value) -> Option<ChatAction> {
    let user = request["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
        })
        .map(Value::to_string)
        .unwrap_or_default();
    for (prompt, reply) in [
        (
            "Return the installed edit recovery response.",
            "Installed edit response.",
        ),
        (
            "Keep this later draft after recovery.",
            "Follow-up delivery response.",
        ),
        (
            "Return the installed delivery recovery response.",
            "Installed delivery response.",
        ),
    ] {
        if user.contains(prompt) {
            return Some(ChatAction::Sse(fake_llm::completion_text_sse(reply)));
        }
    }
    None
}

pub async fn verify(
    access: &gents::ConfigAccess,
    client: &mut Attachment,
    id: &mut u32,
    node_did: &str,
    session: &str,
    client_did: &str,
    root: &Path,
) -> Result<(Value, Value)> {
    let marker: Value =
        serde_json::from_slice(&std::fs::read(root.join("editor-reply-loss.json"))?)?;
    let boundary = std::env::var("GENTS_VSCODE_DELIVERY_BOUNDARY")
        .unwrap_or_else(|_| "after-reply".to_owned());
    ensure!(
        matches!(boundary.as_str(), "before-call" | "after-reply")
            && marker["boundary"] == boundary,
        "the installed journey did not interrupt the selected delivery boundary"
    );
    let request_id = marker["requestId"]
        .as_str()
        .context("missing retained caller identity in dropped reply")?;
    let query = format!("{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}, purpose: {{_eq: \"normal\"}}}}) {{_docID request_id session_id content admission_signature lifecycle_state}} }}",
    gents::graphql::escape_graphql_string(node_did), gents::graphql::escape_graphql_string(session));
    let rows = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let rows = access.execute(&query).await?;
            let requests = rows["data"]["AgentRequest"]
                .as_array()
                .context("missing admitted request rows")?;
            let original: Vec<_> = requests
                .iter()
                .filter(|row| row["request_id"] == request_id)
                .collect();
            let follow_up: Vec<_> = requests
                .iter()
                .filter(|row| row["content"] == "Keep this later draft after recovery.")
                .collect();
            ensure!(
                original.len() == 1 && follow_up.len() == 1,
                "installed recovery duplicated or lost its authored requests: {rows}"
            );
            ensure!(
                original[0]["content"] == "Return the installed delivery recovery response.",
                "installed recovery changed its authored input"
            );
            ensure!(
                requests
                    .iter()
                    .all(|row| row["content"] != "Next draft remains editable."),
                "the unsent draft was submitted"
            );
            ensure!(
                original[0]["admission_signature"]
                    .as_str()
                    .is_some_and(|signature| !signature.is_empty()),
                "installed request has no admission signature"
            );
            if original[0]["lifecycle_state"] == "completed"
                && follow_up[0]["lifecycle_state"] == "completed"
            {
                return Ok::<_, anyhow::Error>(rows);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("installed recovered conversation did not complete")??;
    let marker: Value =
        serde_json::from_slice(&std::fs::read(root.join("editor-fork-reply-loss.json"))?)?;
    let requested: gents_desktop_bridge::types::SessionForkBeforeRequest =
        serde_json::from_value(marker["fork"].clone())?;
    ensure!(
        requested.node_did == node_did
            && requested.session_id == session
            && requested.client_did == client_did,
        "the installed fork crossed its original source or native client"
    );
    let node = node_did.to_owned();
    let child_id = requested.child_session_id.clone();
    let source_id = session.to_owned();
    let (child, source) = access
        .transact_readonly("test.installed_edit_fork", |txn| {
            let node = node.clone();
            let child = child_id.clone();
            let source = source_id.clone();
            Box::pin(async move {
                let child = gents::session::load_agent_session_owner_in_txn(txn, &node, &child)
                    .await?
                    .context("missing retained child")?;
                let source = gents::session::load_agent_session_owner_in_txn(txn, &node, &source)
                    .await?
                    .context("missing original source")?;
                Ok::<_, anyhow::Error>((child, source))
            })
        })
        .await?;
    ensure!(
        child.session.requester_did == source.session.requester_did
            && child.session.agent_id == source.session.agent_id,
        "the recovered fork changed its requester or Agent"
    );
    let provenance = child
        .session
        .provenance
        .as_ref()
        .and_then(|value| value.fork.as_ref())
        .context("the child lost its creation binding")?;
    ensure!(
        provenance.source_session_id == session
            && provenance.before_message_key.as_deref() == Some(requested.message_key.as_str())
            && provenance.edit_client_did.as_deref() == Some(client_did),
        "the child changed its exact retained edit binding"
    );
    let query = format!("{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}, purpose: {{_eq: \"normal\"}}}}) {{_docID request_id content lifecycle_state}} AgentMessage(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{_docID sequence message_key}} }}", gents::graphql::escape_graphql_string(node_did),gents::graphql::escape_graphql_string(&child_id),gents::graphql::escape_graphql_string(node_did),gents::graphql::escape_graphql_string(&child_id));
    let fork_rows = access.execute(&query).await?;
    let requests = fork_rows["data"]["AgentRequest"]
        .as_array()
        .context("missing edited requests")?;
    ensure!(
        requests.len() == 1
            && requests[0]["content"] == "Return the installed edit recovery response."
            && requests[0]["lifecycle_state"] == "completed",
        "the recovered child lost or duplicated its edited request: {fork_rows}"
    );
    let headers = fork_rows["data"]["AgentMessage"]
        .as_array()
        .context("missing copied headers")?;
    let sequences: std::collections::BTreeSet<_> = headers
        .iter()
        .map(|row| row["sequence"].to_string())
        .collect();
    ensure!(
        headers.len() == sequences.len(),
        "fork recovery duplicated a copied prefix sequence"
    );
    let view = read_session(client, id, node_did, &child_id).await?;
    let users: Vec<_> = view["timelineItems"]
        .as_array()
        .context("child timeline")?
        .iter()
        .filter(|item| item["kind"] == "userMessage")
        .map(|item| item["content"].clone())
        .collect();
    ensure!(
        users
            == [
                json!("Return the fixture response."),
                json!("Return the installed edit recovery response.")
            ],
        "the recovered child changed its copied prefix or edited input: {users:?}"
    );
    Ok((
        json!({"lossBoundary": boundary, "records": rows}),
        json!({"binding":marker["fork"].clone(),"childDocId":child.doc_id,"records":fork_rows,"canonicalUsers":users}),
    ))
}
