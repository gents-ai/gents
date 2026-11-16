use std::path::Path;

use anyhow::{ensure, Context, Result};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use serde_json::{json, Value};

pub async fn verify(
    access: &ConfigAccess,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
    opening: &str,
) -> Result<Value> {
    let ui: Value =
        serde_json::from_slice(&tokio::fs::read(root.join("editor-live-delivery-ui.json")).await?)?;
    ensure!(ui["sessionId"] == session);
    let rows = requests(access, node, session).await?;
    for key in ["prompt", "later"] {
        let content = ui[key].as_str().context("authored delivery input")?;
        let matching = rows
            .iter()
            .filter(|row| row.content.as_deref() == Some(content))
            .collect::<Vec<_>>();
        ensure!(matching.len() == 1, "delivery changed or duplicated {key}");
        verify_completed(matching[0], author)?;
    }
    ensure!(
        rows.iter().any(|row| row.request_id == ui["requestId"]),
        "recovery replaced the retained request identity"
    );
    ensure!(
        rows.iter()
            .any(|row| row.content.as_deref() == Some(opening)),
        "recovery removed the original conversation"
    );
    ensure!(
        rows.iter()
            .all(|row| row.content.as_deref() != ui["draft"].as_str()),
        "recovery submitted the unsent draft"
    );
    let marker: Value =
        serde_json::from_slice(&tokio::fs::read(root.join("editor-fork-reply-loss.json")).await?)?;
    let requested: gents_desktop_bridge::types::SessionForkBeforeRequest =
        serde_json::from_value(marker["fork"].clone())?;
    ensure!(
        requested.node_did == node
            && requested.session_id == session
            && requested.client_did == author
            && requested.child_session_id == ui["editedSessionId"],
        "the retained edit crossed its original scope"
    );
    let scope = node.to_owned();
    let child_id = requested.child_session_id.clone();
    let child = access
        .transact_readonly("test.live_delivery_child", |txn| {
            let node = scope.clone();
            let child = child_id.clone();
            Box::pin(async move {
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn, &node, &child,
                )
                .await?
                .context("retained delivery child")
            })
        })
        .await?;
    let provenance = child
        .session
        .provenance
        .as_ref()
        .and_then(|value| value.fork.as_ref())
        .context("retained edit provenance")?;
    ensure!(
        provenance.source_session_id == session
            && provenance.before_message_key.as_deref() == Some(requested.message_key.as_str())
            && provenance.edit_client_did.as_deref() == Some(author),
        "the recovered edit changed its retained source binding"
    );
    let edited = requests(access, node, &requested.child_session_id).await?;
    let [edited] = edited.as_slice() else {
        anyhow::bail!("the retained edit must create one authored request");
    };
    ensure!(edited.content.as_deref() == ui["editPrompt"].as_str());
    verify_completed(edited, author)?;
    let evidence =
        json!({"source":ui,"childDocId":child.doc_id,"editedRequestId":edited.request_id});
    tokio::fs::write(
        root.join("editor-delivery-canonical.json"),
        serde_json::to_vec(&evidence)?,
    )
    .await?;
    tokio::fs::write(
        root.join("editor-fork-canonical.json"),
        serde_json::to_vec(&evidence)?,
    )
    .await?;
    Ok(evidence)
}

async fn requests(
    access: &ConfigAccess,
    node: &str,
    session: &str,
) -> Result<Vec<AgentRequestRow>> {
    let result = access.execute(&format!(
        "{{AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}, purpose: {{_eq: \"normal\"}}}}) {{{}}}}}",
        escape_graphql_string(node), escape_graphql_string(session), gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    Ok(serde_json::from_value(
        result["data"]["AgentRequest"].clone(),
    )?)
}

fn verify_completed(row: &AgentRequestRow, author: &str) -> Result<()> {
    gents_store::identity::request_receipt::verify_request_receipt_signature(row)?;
    ensure!(
        row.lifecycle_state == Some(RequestLifecycleState::Completed)
            && row.admission_signer_did.as_deref() == Some(author)
            && row.requester_did.as_deref() == Some(author),
        "recovered input must complete under its retained author"
    );
    Ok(())
}
