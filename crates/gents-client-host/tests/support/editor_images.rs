use super::fake_llm::{ChatAction, FakeLlm};
use anyhow::{ensure, Context, Result};
use gents::ConfigAccess;
use gents_protocol::{request_input::RequestAttachment, request_lifecycle::RequestLifecycleState};
use serde_json::{json, Value};
use std::path::Path;

pub const REPLY: &str = "Original image bytes received.";

fn image_urls(request: &Value) -> Vec<&str> {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part.pointer("/image_url/url").and_then(Value::as_str))
        .collect()
}

pub fn reply(request: &Value) -> Option<ChatAction> {
    (!image_urls(request).is_empty())
        .then(|| ChatAction::Sse(super::fake_llm::completion_text_sse(REPLY)))
}

pub async fn verify(
    access: &ConfigAccess,
    provider: &FakeLlm,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
) -> Result<Value> {
    let ui: Value = serde_json::from_slice(&std::fs::read(root.join("editor-images-ui.json"))?)?;
    let images: Vec<RequestAttachment> = serde_json::from_value(ui["images"].clone())?;
    ensure!(
        images
            .iter()
            .map(|image| image.media_type.as_str())
            .collect::<Vec<_>>()
            == ["image/png", "image/jpeg", "image/gif", "image/webp"],
        "the editor must exercise all four supported image formats"
    );
    ensure!(
        ui["sessionId"] == session && ui["reopenedImages"] == 4,
        "image reload evidence is incomplete"
    );
    let original_session = ui["originalSessionId"]
        .as_str()
        .context("original image session")?;
    let edited_session = ui["editedSessionId"]
        .as_str()
        .context("edited image session")?;
    ensure!(
        original_session != session
            && edited_session != original_session
            && edited_session != session,
        "editing must create a separate conversation"
    );
    let cases = [
        (session, ""),
        (
            original_session,
            ui["originalPrompt"].as_str().context("original prompt")?,
        ),
        (
            edited_session,
            ui["editedPrompt"].as_str().context("edited prompt")?,
        ),
    ];
    let mut request_ids = Vec::new();
    for (session, content) in cases {
        let rows = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}}}) {{ {} }} }}"#,
        gents::graphql::escape_graphql_string(node),
        gents::graphql::escape_graphql_string(session),
        gents::SIGNED_REQUEST_FIELDS,
    )).await?;
        let rows: Vec<gents_protocol::row::AgentRequestRow> =
            serde_json::from_value(rows["data"]["AgentRequest"].clone())?;
        let captured = rows
            .iter()
            .filter(|row| {
                row.input
                    .as_ref()
                    .is_some_and(|input| !input.attachments.is_empty())
            })
            .collect::<Vec<_>>();
        ensure!(
            captured.len() == 1,
            "images must belong to exactly one submitted request"
        );
        let row = captured[0];
        gents::verify_request_receipt_signature(row)?;
        ensure!(
            row.lifecycle_state == Some(RequestLifecycleState::Completed)
                && row.admission_signer_did.as_deref() == Some(author)
                && row.requester_did.as_deref() == Some(author),
            "image request must complete under the enrolled client's signature"
        );
        ensure!(
            row.content.as_deref() == Some(content),
            "image input content changed"
        );
        let input = row.input.as_ref().context("typed image input")?;
        input.validate_attachments()?;
        ensure!(
            input.attachments == images && input.captured_context.is_empty(),
            "native image input changed the frozen captures"
        );
        request_ids.push(row.request_id.clone());
    }
    let expected = images
        .iter()
        .map(|image| format!("data:{};base64,{}", image.media_type, image.data))
        .collect::<Vec<_>>();
    let requests = provider.captured_chat_requests();
    let delivered = requests
        .iter()
        .filter(|request| {
            image_urls(request) == expected.iter().map(String::as_str).collect::<Vec<_>>()
        })
        .count();
    ensure!(
        delivered == 3,
        "provider must receive the exact frozen images for the original and edited submissions"
    );
    Ok(
        json!({"sessionId":session,"requestIds":request_ids,"originalSessionId":original_session,"editedSessionId":edited_session,"formats":images.iter().map(|image| &image.media_type).collect::<Vec<_>>(),"verifiedSignatures":3,"providerInputOccurrences":delivered,"draftReload":true,"reopenedImages":4}),
    )
}
