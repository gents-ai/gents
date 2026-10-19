use super::fake_llm::FakeLlm;
use anyhow::{ensure, Context, Result};
use gents::ConfigAccess;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use std::path::Path;

pub const PROMPT: &str = "Check the second project selection.";
pub const REPLY: &str = "Second project context received.";
const CONTEXT_HEADER: &str =
    "Editor context (captured text; URI identifies its editor location):\n";
const CAPTURED: &str = "second project captured unsaved context\n";
const LATER: &str = "second project later unsaved context\n";
const DISK: &str = "second project disk context\n";

pub async fn verify(
    access: &ConfigAccess,
    provider: &FakeLlm,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
) -> Result<Value> {
    let ui: Value = serde_json::from_slice(&std::fs::read(root.join("editor-workspace-ui.json"))?)?;
    let result = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}}}) {{ {} }} }}"#,
        gents::graphql::escape_graphql_string(node),
        gents::graphql::escape_graphql_string(session),
        gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(result["data"]["AgentRequest"].clone())?;
    ensure!(rows.len() == 2, "workspace input must be admitted once");
    let row = rows
        .iter()
        .find(|row| {
            row.content
                .as_deref()
                .is_some_and(|content| content.starts_with(PROMPT))
        })
        .context("submitted workspace request")?;
    gents::verify_request_receipt_signature(row)?;
    ensure!(
        row.lifecycle_state == Some(RequestLifecycleState::Completed)
            && row.admission_signer_did.as_deref() == Some(author)
            && row.requester_did.as_deref() == Some(author),
        "workspace request must complete under its enrolled client signature"
    );
    let cwd = row
        .input
        .as_ref()
        .and_then(|input| input.cwd.as_deref())
        .context("workspace directory")?;
    ensure!(
        std::fs::canonicalize(cwd)? == std::fs::canonicalize(root.join("second-project"))?,
        "the request selected a different workspace folder"
    );
    let content = row.content.as_deref().context("workspace input")?;
    ensure!(
        content == PROMPT,
        "captured context was mixed into authored prose"
    );
    let input = row.input.as_ref().context("typed workspace input")?;
    ensure!(
        input.captured_context.len() == 1,
        "one original capture must be submitted"
    );
    let context = serde_json::to_value(&input.captured_context[0])?;
    ensure!(
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
    ensure!(
        !input.captured_context[0].content.contains(LATER.trim()),
        "the later buffer replaced the attachment"
    );
    let captured = provider.captured_chat_requests();
    let matches = captured
        .iter()
        .filter(|request| {
            request["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
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
                    texts.len() == 2
                        && texts[0] == content
                        && texts[1].strip_prefix(CONTEXT_HEADER).is_some_and(|text| {
                            serde_json::from_str::<Value>(text).is_ok_and(|value| value == context)
                        })
                })
            })
        })
        .count();
    ensure!(
        matches == 1,
        "the provider must receive the exact captured input once"
    );
    ensure!(
        std::fs::read_to_string(root.join("second-project/context.rs"))? == DISK,
        "the editor fixture overwrote the unsaved source on disk"
    );
    Ok(json!({
        "nodeDid":node, "sessionId":session, "authorDid":author,
        "requestId":row.request_id, "requestDocId":row.doc_id,
        "state":row.lifecycle_state, "cwd":cwd, "context":context,
        "providerInputOccurrences":matches, "verifiedSignatures":1,
        "diskSourceUnchanged":true,
    }))
}
