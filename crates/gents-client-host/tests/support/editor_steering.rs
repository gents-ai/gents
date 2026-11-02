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
    access: &gents::ConfigAccess,
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
        gents::store::graphql::escape_graphql_string(node_did), gents::store::graphql::escape_graphql_string(session), gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(result["data"]["AgentRequest"].clone())?;
    ensure!(
        rows.len() == if goal_id.is_some() { 4 } else { 3 },
        "steering must retain one active row and two signed versions of the input: {result}"
    );
    for row in &rows {
        gents::verify_request_receipt_signature(row)?;
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
            && steered.lifecycle_state == Some(RequestLifecycleState::Superseded)
            && queued.superseded_by_request_doc_id == steered.doc_id
            && steered.superseded_by_request_doc_id.as_deref() == Some(original_doc_id),
        "the pending row must be promoted and consumed by the original physical request: {result}"
    );
    let rendered = access.execute(&format!(
        r#"{{ RenderedRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ request_doc_id turn_index }} }}"#,
        gents::store::graphql::escape_graphql_string(&active.request_id),
    )).await?;
    let rendered = rendered["data"]["RenderedRequest"]
        .as_array()
        .context("rendered steering requests")?;
    ensure!(
        rendered.len() == 2
            && rendered
                .iter()
                .all(|row| row["request_doc_id"] == original_doc_id)
            && rendered[0]["turn_index"] != rendered[1]["turn_index"],
        "steering must continue the original capture exactly once: {rendered:?}"
    );
    let all_requests = provider.captured_chat_requests();
    let work_requests = all_requests
        .iter()
        .filter(|body| {
            body["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["role"] == "user"
                        && match &message["content"] {
                            Value::String(text) => text == "Return the fixture response.",
                            Value::Array(parts) => parts
                                .iter()
                                .any(|part| part["text"] == "Return the fixture response."),
                            _ => false,
                        }
                })
            })
        })
        .collect::<Vec<_>>();
    let requests = work_requests
        .iter()
        .filter(|body| !has_completed_reply(body))
        .collect::<Vec<_>>();
    ensure!(
        requests.len() == 2,
        "the original request must use exactly two provider rounds"
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
            gents::store::graphql::escape_graphql_string(goal_id),
        )).await?;
        let goals = result["data"]["Goal"].as_array().context("Goal rows")?;
        ensure!(
            goals.len() == 1
                && goals[0]["status"] == "paused"
                && goals[0]["continuation_sequence"] == 1
                && goals[0]["last_continued_from_request_id"] == active.request_id,
            "unexpected Goal continuation: {result}"
        );
        ensure!(
            work_requests.len() == 3 && ui["goalContinuedBeforePause"] == true,
            "installed Goal did not continue before explicit pause"
        );
        Some(child.doc_id.clone())
    } else {
        None
    };
    let provider_input_occurrences = requests
        .iter()
        .map(|body| body["messages"].to_string().matches(STEERING_INPUT).count())
        .sum::<usize>();
    let steered_rounds = requests
        .iter()
        .filter(|body| body["messages"].to_string().contains(STEERING_INPUT))
        .collect::<Vec<_>>();
    ensure!(
        steered_rounds.len() == 1
            && steered_rounds[0]["messages"]
                .to_string()
                .matches(STEERING_INPUT)
                .count()
                == 1,
        "the provider must receive the promoted input exactly once: {requests:?}"
    );
    let capture_count = |body: &Value| {
        body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .filter_map(|text| {
                text.strip_prefix(
                    "Editor context (captured text; URI identifies its editor location):\n",
                )
            })
            .filter_map(|text| serde_json::from_str::<Value>(text).ok())
            .filter(|value| value == &capture)
            .count()
    };
    let captured_parts = capture_count(steered_rounds[0]);
    let all_captured_parts = requests
        .iter()
        .map(|body| capture_count(body))
        .sum::<usize>();
    ensure!(
        captured_parts == 1 && all_captured_parts == 1,
        "the provider lost, duplicated or received the capture before Steer"
    );
    let image_url = format!("data:{};base64,{}", image.media_type, image.data);
    let image_count = |body: &Value| {
        body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .filter(|part| {
                part.pointer("/image_url/url").and_then(Value::as_str) == Some(image_url.as_str())
            })
            .count()
    };
    let all_image_parts = requests.iter().map(|body| image_count(body)).sum::<usize>();
    ensure!(
        image_count(steered_rounds[0]) == 1 && all_image_parts == 1,
        "the provider lost, duplicated or received the image before Steer"
    );
    Ok(json!({
        "nodeDid": node_did, "sessionId": session, "authorDid": client_did,
        "activeRequestDocId": original_doc_id, "queuedRequestDocId": queued.doc_id,
        "steeredRequestDocId": steered.doc_id, "activeState": active.lifecycle_state,
        "queuedState": queued.lifecycle_state, "steeredState": steered.lifecycle_state,
        "queuedReplacedBy": queued.superseded_by_request_doc_id,
        "steeredConsumedBy": steered.superseded_by_request_doc_id,
        "renderedRequests": rendered, "providerInputOccurrences": provider_input_occurrences,
        "providerRequests": requests.len(),
        "verifiedSignatures": rows.len(), "goalContinuationDocId": continuation,
        "context": capture, "providerCaptureOccurrences": all_captured_parts,
        "providerImageOccurrences": all_image_parts,
    }))
}
