use super::{fake_llm, Attachment, ChatAction};
use anyhow::{ensure, Context, Result};
use gents::{graphql::escape_graphql_string, ConfigAccess};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};
use std::path::Path;

pub const PROMPT: &str = "Update the recorded review fixture.";
pub const REPLY: &str = "The recorded file is ready for review.";
const CALL: &str = "recorded-file-write";
const FILE: &str = "review.txt";
const LATER: &str = "Later user changes must stay on disk.\n";

fn contents(marker: &str) -> String {
    format!(
        "{marker}\r\n{}\r\nretained final line",
        "unchanged line\r\n".repeat(240)
    )
}

pub fn prepare(root: &Path) -> Result<()> {
    let before = contents("Recorded original");
    let after = contents("Recorded replacement");
    std::fs::write(root.join(FILE), &before)?;
    std::fs::write(
        root.join("editor-review-fixture.json"),
        serde_json::to_vec(&json!({
            "prompt": PROMPT, "reply": REPLY, "file": FILE, "before": before,
            "after": after, "later": LATER,
            "beforeMarker": "Recorded original", "afterMarker": "Recorded replacement",
        }))?,
    )?;
    Ok(())
}

pub fn reply(request: &Value) -> Option<ChatAction> {
    let messages = request["messages"].as_array()?;
    let selected = messages.iter().any(|message| {
        message["role"] == "user"
            && match &message["content"] {
                Value::String(text) => text == PROMPT,
                Value::Array(parts) => parts.iter().any(|part| part["text"] == PROMPT),
                _ => false,
            }
    });
    if !selected {
        return None;
    }
    let completed = messages
        .iter()
        .any(|message| message["role"] == "tool" && message["tool_call_id"] == CALL);
    Some(ChatAction::Sse(if completed {
        fake_llm::completion_text_sse(REPLY)
    } else {
        fake_llm::tool_call_sse_with_id(
            CALL,
            "write_file",
            &json!({
                "path": FILE, "content": contents("Recorded replacement"),
                "overwrite": true, "raw_json": true,
            })
            .to_string(),
        )
    }))
}

#[allow(clippy::too_many_arguments)]
pub async fn verify(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
) -> Result<Value> {
    let ui: Value = serde_json::from_slice(&std::fs::read(root.join("editor-review-ui.json"))?)?;
    ensure!(
        ui["completeDiffOpened"] == true
            && ui["readOnly"] == true
            && ui["laterWorkingCopyPreserved"] == true,
        "editor review acceptance is incomplete"
    );
    let query = format!("{{ AgentRequest(filter: {{ node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}} }}) {{ {} }} }}",
        escape_graphql_string(node), escape_graphql_string(session), gents::SIGNED_REQUEST_FIELDS);
    let rows = access.execute(&query).await?;
    let rows: Vec<AgentRequestRow> = serde_json::from_value(rows["data"]["AgentRequest"].clone())?;
    let matches = rows
        .iter()
        .filter(|row| row.content.as_deref() == Some(PROMPT))
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "review input was not admitted exactly once"
    );
    let request = matches[0];
    gents::verify_request_receipt_signature(request)?;
    ensure!(
        request.requester_did.as_deref() == Some(author),
        "review authority changed"
    );
    let query = format!("{{ AgentToolCall(filter: {{ request_doc_id: {{_eq: \"{}\"}} }}) {{ _docID lifecycle_state }} }}",
        escape_graphql_string(request.doc_id.as_deref().context("physical review request")?));
    let calls = access.execute(&query).await?;
    let calls = calls["data"]["AgentToolCall"]
        .as_array()
        .context("physical file tool records")?;
    ensure!(
        calls.len() == 1 && calls[0]["lifecycle_state"] == "completed",
        "review must use one completed native file mutation"
    );
    let physical = calls[0]["_docID"]
        .as_str()
        .context("physical review tool")?;
    let retained = access.execute(&format!(
        r#"{{ ToolFileChange(filter: {{ tool_call_doc_id: {{ _eq: "{}" }} }}) {{ _docID file_changes }} }}"#,
        escape_graphql_string(physical),
    )).await?;
    let observations = retained["data"]["ToolFileChange"]
        .as_array()
        .context("retained file observations")?;
    ensure!(
        observations.len() == 1,
        "review must retain exactly one file observation"
    );
    let images = client
        .call(
            *id,
            "desktop_tool_file_changes",
            json!({
                "nodeDid": node, "sessionId": session, "requestId": request.request_id,
                "toolCallDocId": physical,
            }),
        )
        .await;
    *id += 1;
    ensure!(
        images == observations[0]["file_changes"],
        "client transport changed retained images"
    );
    ensure!(
        images["changes"][0]["before"]["kind"] == "file"
            && images["changes"][0]["after"]["kind"] == "file",
        "both immutable file versions must be retained"
    );
    ensure!(
        node != author,
        "review must cross enrolled client identities"
    );
    let delivered = access
        .execute(&format!(
            r#"{{ FileImageHydrationRequest(filter: {{ tool_call_doc_id: {{ _eq: "{}" }} }}) {{
            request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id
            status observation_doc_id outcome_signer_did outcome_signature
        }} }}"#,
            escape_graphql_string(physical),
        ))
        .await?;
    let receipts = delivered["data"]["FileImageHydrationRequest"]
        .as_array()
        .context("remote review delivery receipts")?;
    ensure!(
        receipts.len() == 1,
        "review demand must be reused after delivery"
    );
    let receipt = &receipts[0];
    let mut demand = gents_protocol::file_image_hydration::FileImageDemand {
        admission: gents_protocol::session_hydration::HydrationRequest {
            request_key: String::new(),
            peer_id: receipt["peer_id"].as_str().context("review peer")?.into(),
            requester_did: author.into(),
            node_did: node.into(),
            session_id: session.into(),
        },
        request_doc_id: request
            .doc_id
            .as_deref()
            .context("physical review request")?
            .into(),
        tool_call_doc_id: physical.into(),
    };
    demand.admission.request_key = demand.derive_request_key();
    for (field, expected) in demand.reference_fields() {
        ensure!(
            receipt[field].as_str() == Some(expected),
            "review receipt changed {field}"
        );
    }
    ensure!(
        receipt["status"] == "served"
            && receipt["observation_doc_id"] == observations[0]["_docID"]
            && receipt["outcome_signer_did"] == node
            && receipt["outcome_signature"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
        "review receipt did not identify the delivered physical image"
    );
    ensure!(
        std::fs::read_to_string(root.join(FILE))? == LATER,
        "review overwrote later user changes"
    );
    Ok(
        json!({"sessionId":session, "requestId":request.request_id, "physicalTool":physical,
        "enrolledClientRead":true, "retainedImagesUnchanged":true, "workingCopyPreserved":true,
        "scopedDeliveryReceipt":true, "exactImageIdentity":true, "demandReused":true}),
    )
}
