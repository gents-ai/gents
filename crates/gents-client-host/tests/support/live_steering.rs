use std::path::Path;

use anyhow::{ensure, Context, Result};
use gents_protocol::request_input::QueueDelivery;
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
    request: &str,
    author: &str,
    goal: Option<&str>,
) -> Result<()> {
    let ui: Value =
        serde_json::from_slice(&tokio::fs::read(root.join("editor-steering-ui.json")).await?)?;
    let response = access.execute(&format!(
        "{{AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}, purpose: {{_eq: \"normal\"}}}}) {{{} superseded_by_request_doc_id}} Goal(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{{}}}}}",
        escape_graphql_string(node), escape_graphql_string(session), gents::SIGNED_REQUEST_FIELDS,
        escape_graphql_string(node), escape_graphql_string(session), gents_protocol::goal::GOAL_FIELDS,
    )).await?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    let active = rows
        .iter()
        .find(|row| row.request_id == request)
        .context("steered active request")?;
    ensure!(active.lifecycle_state == Some(RequestLifecycleState::Completed));
    let find = |delivery| {
        rows.iter().find(|row| {
            row.content.as_deref() == Some(super::super::STEERING_INPUT)
                && row
                    .input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .is_some_and(|queue| queue.delivery == delivery)
        })
    };
    let queued = find(QueueDelivery::Queue).context("queued source input")?;
    let steered = find(QueueDelivery::Steer).context("steered source input")?;
    for row in [active, queued, steered] {
        gents_store::identity::request_receipt::verify_request_receipt_signature(row)?;
        ensure!(row.admission_signer_did.as_deref() == Some(author));
    }
    ensure!(
        queued.lifecycle_state == Some(RequestLifecycleState::Superseded)
            && steered.lifecycle_state == Some(RequestLifecycleState::Superseded)
            && queued.superseded_by_request_doc_id == steered.doc_id
            && steered.superseded_by_request_doc_id == active.doc_id
    );
    let captured = &queued
        .input
        .as_ref()
        .context("queued input")?
        .captured_context;
    ensure!(
        captured.len() == 1
            && captured[0].content == "queued captured unsaved context\n"
            && steered.input.as_ref().map(|input| &input.captured_context) == Some(captured)
    );
    ensure!(
        queued
            .input
            .as_ref()
            .is_some_and(|input| input.attachments.is_empty())
            && steered
                .input
                .as_ref()
                .is_some_and(|input| input.attachments.is_empty())
    );
    if let Some(goal) = goal {
        ensure!(response["data"]["Goal"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["goal_id"] == goal && row["status"] == "paused")));
        ensure!(rows
            .iter()
            .any(|row| row.caused_by_trigger_id.as_deref() == Some(goal)
                && row.caused_by_parent_request_id.as_deref() == Some(request)
                && row.lifecycle_state == Some(RequestLifecycleState::Completed)));
    }
    let evidence = json!({"nodeDid":node,"sessionId":session,"requestId":request,
        "goalId":goal,"ui":ui,"capture":captured,"textOnly":true});
    tokio::fs::write(
        root.join("editor-steering-canonical.json"),
        serde_json::to_vec(&evidence)?,
    )
    .await?;
    Ok(())
}
