use std::path::Path;

use anyhow::{ensure, Context, Result};
use base64::Engine;
use gents_protocol::file_changes::{FileImage, ToolFileChanges};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use serde_json::{json, Value};

use super::Attachment;

const REVIEW_FILE: &str = "review.txt";
const REVIEW_PROMPT: &str = "Call edit_file with path review.txt, old_text Recorded original, new_text Recorded replacement, and raw_json true. Preserve every other byte and do not edit any other file. After the edit succeeds, give a brief confirmation.";
const REVIEW_LATER: &str = "Later user changes must stay on disk.\n";

fn review_contents(marker: &str) -> String {
    format!(
        "{marker}\r\n{}\r\nretained final line",
        "unchanged line\r\n".repeat(240)
    )
}

pub async fn prepare_review(root: &Path) -> Result<()> {
    let before = review_contents("Recorded original");
    let after = review_contents("Recorded replacement");
    tokio::fs::write(root.join(REVIEW_FILE), &before).await?;
    tokio::fs::write(
        root.join("editor-review-fixture.json"),
        serde_json::to_vec(&json!({
            "prompt": REVIEW_PROMPT,
            "file": REVIEW_FILE,
            "before": before,
            "after": after,
            "later": REVIEW_LATER,
            "beforeMarker": "Recorded original",
            "afterMarker": "Recorded replacement",
        }))?,
    )
    .await?;
    Ok(())
}

pub async fn verify_review(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
) -> Result<Value> {
    let ui: Value =
        serde_json::from_slice(&tokio::fs::read(root.join("editor-review-ui.json")).await?)?;
    ensure!(
        ui["sessionId"] == session
            && ui["completeDiffOpened"] == true
            && ui["readOnly"] == true
            && ui["laterWorkingCopyPreserved"] == true
            && ui["reloadPreservesDraftAndWorkingCopy"] == true,
        "editor did not preserve its reviewed images, draft, and working copy: {ui}"
    );
    ensure!(
        ui["answer"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty()),
        "the editor did not observe the live answer"
    );
    let response = access
        .execute(&format!(
            "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{{}}} }}",
            escape_graphql_string(node),
            escape_graphql_string(session),
            gents::SIGNED_REQUEST_FIELDS,
        ))
        .await?;
    let requests: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    let request = requests
        .iter()
        .find(|request| request.content.as_deref() == Some(REVIEW_PROMPT))
        .context("the editor's submitted file edit")?;
    gents_store::identity::request_receipt::verify_request_receipt_signature(request)?;
    ensure!(
        request.lifecycle_state == Some(RequestLifecycleState::Completed)
            && request.requester_did.as_deref() == Some(author)
            && request.admission_signer_did.as_deref() == Some(author),
        "the file edit did not complete under the enrolled client's signature"
    );
    let draft = ui["draft"].as_str().context("retained editor draft")?;
    ensure!(
        requests
            .iter()
            .all(|request| request.content.as_deref() != Some(draft)),
        "the editor submitted its retained draft"
    );
    let response = access
        .execute(&format!(
            "{{ AgentToolCall(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{_docID lifecycle_state}} }}",
            escape_graphql_string(
                request
                    .doc_id
                    .as_deref()
                    .context("physical file edit request")?
            ),
        ))
        .await?;
    let calls = response["data"]["AgentToolCall"]
        .as_array()
        .context("persisted file tools")?;
    let before = review_contents("Recorded original");
    let after = review_contents("Recorded replacement");
    let expected_root = tokio::fs::canonicalize(root).await?;
    let mut retained = None;
    for call in calls {
        if call["lifecycle_state"] != "completed" {
            continue;
        }
        let physical = call["_docID"].as_str().context("physical file tool")?;
        let result = client
            .try_call(
                *id,
                "desktop_tool_file_changes",
                json!({
                    "nodeDid": node,
                    "sessionId": session,
                    "requestId": request.request_id,
                    "toolCallDocId": physical,
                }),
            )
            .await?;
        *id += 1;
        if result.is_null() {
            continue;
        }
        let changes: ToolFileChanges = serde_json::from_value(result)?;
        for change in &changes.changes {
            if Path::new(&change.path).file_name() != Some(REVIEW_FILE.as_ref()) {
                continue;
            }
            let FileImage::File { data: original, .. } = &change.before else {
                continue;
            };
            let FileImage::File { data: updated, .. } = &change.after else {
                continue;
            };
            if base64::engine::general_purpose::STANDARD.decode(original)? == before.as_bytes()
                && base64::engine::general_purpose::STANDARD.decode(updated)? == after.as_bytes()
            {
                ensure!(
                    tokio::fs::canonicalize(&change.root).await? == expected_root,
                    "the reviewed mutation escaped the configured root"
                );
                retained = Some(changes.clone());
            }
        }
    }
    let retained = retained.context("the native file owner did not retain both exact images")?;
    ensure!(
        tokio::fs::read(root.join(REVIEW_FILE)).await? == REVIEW_LATER.as_bytes(),
        "review changed the later working copy"
    );
    Ok(json!({
        "sessionId": session,
        "requestId": request.request_id,
        "enrolledClientRead": node != author,
        "retainedImages": retained,
        "workingCopyPreserved": true,
        "draftPreserved": true,
    }))
}
