use super::{fake_llm::FakeLlm, STEERING_INPUT, STEERING_REPLY};
use anyhow::{ensure, Context as _, Result};
use serde_json::{json, Value};
use std::path::Path;

pub struct Context<'a> {
    pub root: &'a Path,
    pub node_did: &'a str,
    pub session: &'a str,
    pub original_doc_id: &'a str,
    pub client_did: &'a str,
    pub goal_id: Option<&'a str>,
}

pub fn has_completed_reply(request: &Value) -> bool {
    request["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["role"] == "assistant"
                && message["content"].to_string().contains(STEERING_REPLY)
        })
    })
}

pub async fn verify(
    access: &gents_store::config_client::ConfigAccess,
    provider: &FakeLlm,
    context: Context<'_>,
) -> Result<Value> {
    let Context {
        root,
        node_did,
        session,
        original_doc_id,
        client_did,
        goal_id,
    } = context;
    use gents_protocol::request_input::QueueDelivery;
    use gents_protocol::request_lifecycle::RequestLifecycleState;

    let result = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, purpose: {{ _eq: "normal" }} }}) {{ {} superseded_by_request_doc_id }} }}"#,
        gents_store::graphql::escape_graphql_string(node_did), gents_store::graphql::escape_graphql_string(session), gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(result["data"]["AgentRequest"].clone())?;
    for row in &rows {
        gents_store::identity::request_receipt::verify_request_receipt_signature(row)?;
        if row.caused_by_trigger_kind.as_deref() == Some("goal") {
            ensure!(goal_id.is_some(), "unexpected Goal continuation");
            continue;
        }
        ensure!(
            row.admission_signer_did.as_deref() == Some(client_did)
                && row.requester_did.as_deref() == Some(client_did),
            "steering changed the enrolled author"
        );
    }
    let active = rows
        .iter()
        .find(|row| row.doc_id.as_deref() == Some(original_doc_id))
        .context("original request")?;
    let input = |delivery| {
        rows.iter().find(|row| {
            row.content.as_deref() == Some(STEERING_INPUT)
                && row
                    .input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .is_some_and(|queue| queue.delivery == delivery)
        })
    };
    let queued = input(QueueDelivery::Queue).context("original queued input")?;
    let steered = input(QueueDelivery::Steer).context("promoted input")?;
    let ui: Value = serde_json::from_slice(&std::fs::read(root.join("editor-steering-ui.json"))?)?;
    let image: gents_protocol::request_input::RequestAttachment =
        serde_json::from_value(ui["image"].clone())?;
    for row in [queued, steered] {
        let input = row.input.as_ref().context("image-bearing steering input")?;
        input.validate_attachments()?;
        ensure!(
            input.attachments == [image.clone()],
            "Steer changed or dropped the frozen image"
        );
    }
    let captured = &queued
        .input
        .as_ref()
        .context("queued input")?
        .captured_context;
    ensure!(
        captured.len() == 1
            && steered.input.as_ref().map(|input| &input.captured_context) == Some(captured),
        "Steer changed or dropped the queued capture"
    );
    let capture = serde_json::to_value(&captured[0])?;
    ensure!(
        capture["content"] == "queued captured unsaved context\n"
            && capture["uri"] == ui["contextUri"]
            && capture["kind"] == "selection"
            && capture["source"] == "editor"
            && capture["unsaved"] == true
            && capture["version"].as_u64().is_some_and(|v| v > 1)
            && capture["range"]["start"] == json!({"line":0,"character":0})
            && capture["range"]["end"] == json!({"line":1,"character":0}),
        "Steer did not retain the submitted editor observations"
    );
    ensure!(
        active.lifecycle_state == Some(RequestLifecycleState::Completed)
            && queued.lifecycle_state == Some(RequestLifecycleState::Superseded)
            && steered.lifecycle_state == Some(RequestLifecycleState::Superseded),
        "the original request did not complete with the queued input consumed: {result}"
    );
    let continuation = if let Some(goal_id) = goal_id {
        let child = rows
            .iter()
            .find(|row| row.caused_by_trigger_kind.as_deref() == Some("goal"))
            .context("continued Goal request")?;
        ensure!(
            child.lifecycle_state == Some(RequestLifecycleState::Completed),
            "Goal child did not complete"
        );
        ensure!(
            child.caused_by_parent_request_doc_id.as_deref() == Some(original_doc_id)
                && child.runtime_source_request_id.as_deref() == Some(active.request_id.as_str())
                && child.runtime_issuer_did.as_deref() == Some(node_did)
                && child.admission_signer_did.as_deref() == Some(node_did),
            "Goal did not continue from the steered physical request"
        );
        let result = access.execute(&format!(
            r#"{{ Goal(filter: {{ goal_id: {{ _eq: "{}" }} }}) {{ goal_id status continuation_sequence last_continued_from_request_id }} }}"#,
            gents_store::graphql::escape_graphql_string(goal_id),
        )).await?;
        let goals = result["data"]["Goal"].as_array().context("Goal rows")?;
        ensure!(
            goals.len() == 1
                && goals[0]["status"] == "paused"
                && goals[0]["last_continued_from_request_id"] == active.request_id,
            "unexpected Goal continuation: {result}"
        );
        ensure!(
            ui["goalContinuedBeforePause"] == true,
            "installed Goal did not continue before explicit pause"
        );
        Some(child.doc_id.clone())
    } else {
        None
    };
    let image_url = format!("data:{};base64,{}", image.media_type, image.data);
    ensure!(
        provider.captured_chat_requests().iter().any(|body| {
            let parts: Vec<_> = body["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|message| message["role"] == "user")
                .filter_map(|message| message["content"].as_array())
                .flatten()
                .collect();
            parts.iter().any(|part| part["text"] == STEERING_INPUT)
                && parts.iter().any(|part| {
                    part.pointer("/image_url/url").and_then(Value::as_str)
                        == Some(image_url.as_str())
                })
                && parts.iter().any(|part| {
                    part["text"]
                        .as_str()
                        .and_then(|text| {
                            text.strip_prefix(
                            "Editor context (captured text; URI identifies its editor location):\n",
                        )
                        })
                        .and_then(|text| serde_json::from_str::<Value>(text).ok())
                        .is_some_and(|value| value == capture)
                })
        }),
        "the provider payload lost the promoted text, image or captured editor context"
    );
    Ok(json!({
        "nodeDid": node_did, "sessionId": session, "authorDid": client_did,
        "activeRequestDocId": original_doc_id, "queuedRequestDocId": queued.doc_id,
        "steeredRequestDocId": steered.doc_id, "activeState": active.lifecycle_state,
        "queuedState": queued.lifecycle_state, "steeredState": steered.lifecycle_state,
        "queuedReplacedBy": queued.superseded_by_request_doc_id,
        "steeredConsumedBy": steered.superseded_by_request_doc_id,
        "verifiedSignatures": rows.len(), "goalContinuationDocId": continuation,
        "context": capture, "promotedImageBytesPreserved": true,
    }))
}
