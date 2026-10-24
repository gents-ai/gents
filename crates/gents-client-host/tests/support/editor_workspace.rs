use super::fake_llm::FakeLlm;
use crate::support::TestResult;
use gents::ConfigAccess;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use std::path::Path;

pub const PROMPT: &str = "Check the second project selection.";
pub const REPLY: &str = "Second project context received.";
pub const ATTACHMENT_ONLY_REPLY: &str = "Attached context received.";
const CONTEXT_HEADER: &str =
    "Editor context (captured text; URI identifies its editor location):\n";
pub const CAPTURED: &str = "second project captured unsaved context\n";
const LATER: &str = "second project later unsaved context\n";
const DISK: &str = "second project disk context\n";

pub async fn verify(
    access: &ConfigAccess,
    provider: &FakeLlm,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
) -> TestResult<Value> {
    let ui: Value = serde_json::from_slice(&std::fs::read(root.join("editor-workspace-ui.json"))?)?;
    let rows = requests(access, node, session).await?;
    assert!(rows.len() == 2, "workspace input must be admitted once");
    let row = rows
        .iter()
        .find(|row| {
            row.content
                .as_deref()
                .is_some_and(|content| content.starts_with(PROMPT))
        })
        .expect("submitted workspace request");
    gents::verify_request_receipt_signature(row)?;
    assert!(
        row.lifecycle_state == Some(RequestLifecycleState::Completed)
            && row.admission_signer_did.as_deref() == Some(author)
            && row.requester_did.as_deref() == Some(author),
        "workspace request must complete under its enrolled client signature"
    );
    let cwd = row
        .input
        .as_ref()
        .and_then(|input| input.cwd.as_deref())
        .expect("workspace directory");
    assert!(
        std::fs::canonicalize(cwd)? == std::fs::canonicalize(root.join("second-project"))?,
        "the request selected a different workspace folder"
    );
    let content = row.content.as_deref().expect("workspace input");
    assert!(
        content == PROMPT,
        "captured context was mixed into authored prose"
    );
    let input = row.input.as_ref().expect("typed workspace input");
    assert!(
        input.captured_context.len() == 1,
        "one original capture must be submitted"
    );
    let context = serde_json::to_value(&input.captured_context[0])?;
    assert!(
        context["kind"] == "selection"
            && context["source"] == "editor"
            && context["content"] == CAPTURED
            && context["unsaved"] == true
            && context["version"]
                .as_u64()
                .is_some_and(|version| version > 1)
            && context["uri"] == ui["contextUri"]
            && context["workspaceUri"] == ui["selectedFolderUri"]
            && context["range"]["start"] == json!({"line":0,"character":0})
            && context["range"]["end"] == json!({"line":1,"character":0}),
        "captured context lost its source, version, range or unsaved contents: {context}"
    );
    assert!(
        !input.captured_context[0].content.contains(LATER.trim()),
        "the later buffer replaced the attachment"
    );
    let matches = provider_occurrences(provider, content, &context);
    assert!(
        matches == 1,
        "the provider must receive the exact captured input once"
    );
    let edited_session = ui["editedSessionId"]
        .as_str()
        .expect("edited chat identity");
    let edited_prompt = ui["editedPrompt"].as_str().expect("edited prose");
    assert!(
        edited_session != session,
        "message editing overwrote the source chat"
    );
    let edited_rows = requests(access, node, edited_session).await?;
    let edited = edited_rows
        .iter()
        .find(|row| row.content.as_deref() == Some(edited_prompt))
        .expect("edited branch request");
    verify_completed(edited, author)?;
    assert!(
        edited.input.as_ref().map(|input| &input.captured_context) == Some(&input.captured_context),
        "message editing replaced the original capture"
    );
    assert!(
        provider_occurrences(provider, edited_prompt, &context) == 1,
        "edited snapshot must reach the provider once"
    );
    let attachment_session = ui["attachmentOnlySessionId"]
        .as_str()
        .expect("attachment-only chat identity");
    assert!(
        attachment_session != session && attachment_session != edited_session,
        "attachment-only input reused another chat"
    );
    let attachment_rows = requests(access, node, attachment_session).await?;
    assert!(
        attachment_rows.len() == 1,
        "attachment-only input must be admitted once"
    );
    let attachment = &attachment_rows[0];
    verify_completed(attachment, author)?;
    assert!(
        attachment.content.as_deref() == Some(""),
        "attachment-only input fabricated prose"
    );
    let attachment_input = attachment
        .input
        .as_ref()
        .expect("attachment-only typed input");
    assert!(
        attachment_input.captured_context.len() == 1,
        "attachment-only capture count"
    );
    let attachment_context = serde_json::to_value(&attachment_input.captured_context[0])?;
    assert!(
        attachment_context["content"] == CAPTURED
            && attachment_context["uri"] == context["uri"]
            && attachment_context["workspaceUri"] == context["workspaceUri"]
            && attachment_context["range"] == context["range"]
            && attachment_context["unsaved"] == true
            && attachment_context["version"]
                .as_u64()
                .is_some_and(|version| version > 1),
        "attachment-only submission lost its captured snapshot"
    );
    assert!(
        provider_occurrences(provider, "", &attachment_context) == 1,
        "attachment-only snapshot must reach the provider once"
    );
    let title_input = format!("First user request:\n{}\n\n", CAPTURED.trim());
    let title_requests = provider
        .captured_chat_requests()
        .iter()
        .filter(|request| {
            request["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|message| {
                    message["role"] == "user"
                        && match &message["content"] {
                            Value::String(text) => text.starts_with(&title_input),
                            Value::Array(parts) => parts.iter().any(|part| {
                                part["text"]
                                    .as_str()
                                    .is_some_and(|text| text.starts_with(&title_input))
                            }),
                            _ => false,
                        }
                })
        })
        .count();
    assert!(
        title_requests == 1,
        "attachment-only title must use the frozen parent input once"
    );
    assert!(
        std::fs::read_to_string(root.join("second-project/context.rs"))? == DISK,
        "the editor fixture overwrote the unsaved source on disk"
    );
    Ok(json!({
        "nodeDid":node, "sessionId":session, "authorDid":author,
        "requestId":row.request_id, "requestDocId":row.doc_id,
        "state":row.lifecycle_state, "cwd":cwd, "context":context,
        "providerInputOccurrences":matches, "verifiedSignatures":3,
        "editedSessionId": edited_session, "attachmentOnlySessionId": attachment_session,
        "originalConversationPreserved":true, "attachmentOnlyProse":"",
        "attachmentOnlyTitleInputVerified":true,
        "diskSourceUnchanged":true,
    }))
}

async fn requests(
    access: &ConfigAccess,
    node: &str,
    session: &str,
) -> TestResult<Vec<gents_protocol::row::AgentRequestRow>> {
    let result = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}}}) {{ {} }} }}"#,
        gents::graphql::escape_graphql_string(node),
        gents::graphql::escape_graphql_string(session),
        gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    Ok(serde_json::from_value(
        result["data"]["AgentRequest"].clone(),
    )?)
}

fn verify_completed(row: &gents_protocol::row::AgentRequestRow, author: &str) -> TestResult<()> {
    gents::verify_request_receipt_signature(row)?;
    assert!(
        row.lifecycle_state == Some(RequestLifecycleState::Completed)
            && row.admission_signer_did.as_deref() == Some(author)
            && row.requester_did.as_deref() == Some(author),
        "captured input did not complete under its enrolled client signature"
    );
    Ok(())
}

fn provider_occurrences(provider: &FakeLlm, content: &str, context: &Value) -> usize {
    let captured = provider.captured_chat_requests();
    captured
        .iter()
        .flat_map(|request| request["messages"].as_array().into_iter().flatten())
        .filter(|message| {
            if message["role"] != "user" {
                return false;
            }
            let texts = match &message["content"] {
                Value::String(text) => vec![text.as_str()],
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect(),
                _ => return false,
            };
            texts.len() == if content.is_empty() { 1 } else { 2 }
                && (content.is_empty() || texts[0] == content)
                && texts
                    .last()
                    .unwrap()
                    .strip_prefix(CONTEXT_HEADER)
                    .is_some_and(|text| {
                        serde_json::from_str::<Value>(text).is_ok_and(|value| &value == context)
                    })
        })
        .count()
}
