use super::{editor_review, fake_llm, Attachment, ChatAction};
use anyhow::{ensure, Context, Result};
use base64::Engine;
use gents::{graphql::escape_graphql_string, ConfigAccess, NodeIdentity};
use gents_protocol::{
    goal_control::{
        GoalControlOutcome, GoalOperation, SessionGoalControl, SessionGoalControlReceipt,
    },
    request_lifecycle::RequestLifecycleState,
    row::AgentRequestRow,
};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc, time::Duration};

pub const OBJECTIVE: &str = "Prepare two recorded edits for saved Undo.";
pub const REFUSALS: &str = "Prepare the protected Undo targets.";
pub const PARTIAL: &str = "Prepare the partial save Undo targets.";
pub const DRAFT: &str = "Keep this unsent Undo draft";
pub const RUNNING: &str = "Recorded edits are ready. Waiting for the paused Goal.";
pub const FINISHED: &str =
    "Recorded edits are ready. Waiting for the paused Goal. Current work finished.";
const READY: &str = "Protected Undo targets are ready.";
const PARTIAL_READY: &str = "Partial save Undo targets are ready.";

fn cases() -> Value {
    json!([
        {"prompt":OBJECTIVE,"reply":FINISHED,"files":[
            {"path":"undo-success-a.txt","before":"Original success A λ\n","after":"Recorded success A λ\n"},
            {"path":"undo-success-b.txt","before":"Original success B β\n","after":"Recorded success B β\n"}
        ]},
        {"prompt":REFUSALS,"reply":READY,"files":[
            {"path":"undo-dirty.txt","before":"Original dirty\n","after":"Recorded dirty\n","later":"Unsent user buffer\n"},
            {"path":"undo-disk.txt","before":"Original disk\n","after":"Recorded disk\n","later":"Later disk changes\n"},
            {"path":"undo-created.txt","before":null,"after":"Created recorded target\n"},
            {"path":"undo-locked.txt","before":"Original locked\n","after":"Recorded locked\n"}
        ]},
        {"prompt":PARTIAL,"reply":PARTIAL_READY,"files":[
            {"path":"undo-partial-a.txt","before":"Original partial A\n","after":"Recorded partial A\n"},
            {"path":"undo-partial-b.txt","before":"Original partial B\n","after":"Recorded partial B\n","later":"External save conflict\n"}
        ]}
    ])
}

pub fn prepare(root: &Path) -> Result<()> {
    let cases = cases();
    for case in cases.as_array().context("Undo cases")? {
        for file in case["files"].as_array().context("Undo files")? {
            if let Some(before) = file["before"].as_str() {
                std::fs::write(root.join(text(file, "path")?), before)?;
            }
        }
    }
    std::fs::write(
        root.join("editor-undo-fixture.json"),
        serde_json::to_vec_pretty(&json!({"cases":cases,"draft":DRAFT,"running":RUNNING}))?,
    )?;
    Ok(())
}

pub struct Provider {
    gate: Arc<tokio::sync::Semaphore>,
}
impl Default for Provider {
    fn default() -> Self {
        Self {
            gate: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}
impl Provider {
    pub fn release(&self) {
        self.gate.add_permits(1);
    }
    pub fn reply(
        &self,
        request: &Value,
        emitted: &tokio::sync::watch::Sender<Option<std::time::Instant>>,
    ) -> Option<ChatAction> {
        let messages = request["messages"].as_array()?;
        let cases = cases();
        for (case_index, case) in cases.as_array()?.iter().enumerate() {
            if !messages.iter().any(|message| {
                message["role"] == "user"
                    && (message["content"] == case["prompt"]
                        || message["content"].as_array().is_some_and(|parts| {
                            parts.iter().any(|part| part["text"] == case["prompt"])
                        }))
            }) {
                continue;
            }
            for (file_index, file) in case["files"].as_array()?.iter().enumerate() {
                let call = format!("recorded-undo-{case_index}-{file_index}");
                if messages
                    .iter()
                    .any(|message| message["role"] == "tool" && message["tool_call_id"] == call)
                {
                    continue;
                }
                return Some(ChatAction::Sse(fake_llm::tool_call_sse_with_id(
                    &call,
                    "write_file",
                    &json!({"path":file["path"],"content":file["after"],"overwrite":true,"raw_json":true}).to_string(),
                )));
            }
            return Some(if case_index == 0 {
                ChatAction::GatedSse(
                    vec![(
                        Duration::ZERO,
                        format!(
                            "data: {}\n\n",
                            json!({"choices":[{"delta":{"content":RUNNING},"finish_reason":null}],"usage":null})
                        ),
                    )],
                    Arc::clone(&self.gate),
                    fake_llm::completion_text_sse(" Current work finished."),
                    emitted.clone(),
                )
            } else {
                ChatAction::Sse(fake_llm::completion_text_sse(case["reply"].as_str()?))
            });
        }
        None
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn verify(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    root: &Path,
    node: &str,
    author: &str,
    verifier: &dyn NodeIdentity,
    provider: &fake_llm::FakeLlm,
) -> Result<Value> {
    let ui: Value = serde_json::from_slice(&std::fs::read(root.join("editor-undo-ui.json"))?)?;
    for name in [
        "twoFilesRestoredAndSaved",
        "savedEventsObserved",
        "globalThenIndividualRefused",
        "reloadPreserved",
        "crashRestartPreserved",
        "dirtyPreserved",
        "diskPreserved",
        "createdPreserved",
        "lockPreserved",
        "partialSavePreserved",
        "draftAndPausedGoalPreserved",
        "retainedReviewReopened",
    ] {
        ensure!(ui[name] == true, "installed Undo evidence omitted {name}");
    }
    let value = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}}}) {{ {} }} Goal(filter: {{node_did: {{_eq: "{}"}}}}) {{ {} }} AgentSessionGoalControl(filter: {{node_did: {{_eq: "{}"}}}}) {{ intent_json receipt_json }} }}"#,
        escape_graphql_string(node), gents::SIGNED_REQUEST_FIELDS,
        escape_graphql_string(node), gents::goal::GOAL_FIELDS, escape_graphql_string(node),
    )).await?;
    let rows: Vec<AgentRequestRow> = serde_json::from_value(value["data"]["AgentRequest"].clone())?;
    let mut sessions = std::collections::BTreeSet::new();
    let mut observations = Vec::new();
    for case in cases().as_array().context("Undo cases")? {
        let matches = rows
            .iter()
            .filter(|row| row.content.as_deref() == case["prompt"].as_str())
            .collect::<Vec<_>>();
        ensure!(
            matches.len() == 1,
            "Undo input was not admitted exactly once"
        );
        let request = matches[0];
        gents::verify_request_receipt_signature(request)?;
        ensure!(
            request.requester_did.as_deref() == Some(author)
                && request.lifecycle_state == Some(RequestLifecycleState::Completed),
            "Undo request lost its authenticated completed owner"
        );
        sessions.insert(request.session_id.clone().context("Undo session")?);
        let physical_request = request.doc_id.as_deref().context("Undo physical request")?;
        let calls = access.execute(&format!(
            r#"{{ AgentToolCall(filter: {{request_doc_id: {{_eq: "{}"}}}}) {{ _docID lifecycle_state }} }}"#,
            escape_graphql_string(physical_request),
        )).await?;
        let calls = calls["data"]["AgentToolCall"]
            .as_array()
            .context("Undo physical tools")?;
        ensure!(
            calls.len() == case["files"].as_array().context("Undo files")?.len(),
            "Undo native capture count changed"
        );
        let mut seen = std::collections::BTreeSet::new();
        for call in calls {
            ensure!(
                call["lifecycle_state"] == "completed",
                "Undo file capture did not complete"
            );
            let physical = text(call, "_docID")?;
            let retained = access.execute(&format!(
                r#"{{ ToolFileChange(filter: {{tool_call_doc_id: {{_eq: "{}"}}}}) {{ _docID file_changes }} }}"#,
                escape_graphql_string(physical),
            )).await?;
            let retained = retained["data"]["ToolFileChange"]
                .as_array()
                .context("Undo retained captures")?;
            ensure!(
                retained.len() == 1,
                "Undo tool lost its unique retained capture"
            );
            let images = client.call(*id, "desktop_tool_file_changes", json!({"nodeDid":node,"sessionId":request.session_id,"requestId":request.request_id,"toolCallDocId":physical})).await;
            *id += 1;
            ensure!(
                images == retained[0]["file_changes"],
                "Undo read substituted retained images"
            );
            let changes = images["changes"].as_array().context("Undo image changes")?;
            ensure!(changes.len() == 1, "Undo file capture grouping changed");
            let change = &changes[0];
            let path = text(change, "path")?;
            ensure!(
                seen.insert(path.to_owned()),
                "Undo fixture repeated a physical file"
            );
            let file = case["files"]
                .as_array()
                .context("Undo files")?
                .iter()
                .find(|file| file["path"] == path)
                .context("unexpected Undo capture")?;
            for side in ["before", "after"] {
                if let Some(content) = file[side].as_str() {
                    ensure!(
                        change[side]["kind"] == "file",
                        "Undo lost a retained file version"
                    );
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(text(&change[side], "data")?)?;
                    ensure!(
                        bytes == content.as_bytes(),
                        "Undo retained {side} bytes changed"
                    );
                } else {
                    ensure!(
                        change[side]["kind"] == "missing",
                        "created target acquired an invented preimage"
                    );
                }
            }
            let receipts = editor_review::review_receipts(access, physical).await?;
            ensure!(
                !receipts.is_empty() && node != author,
                "Undo bypassed enrolled image delivery"
            );
            for receipt in receipts {
                let demand = gents_protocol::file_image_hydration::FileImageDemand {
                    admission: gents_protocol::session_hydration::HydrationRequest {
                        request_key: text(&receipt, "request_key")?.into(),
                        peer_id: text(&receipt, "peer_id")?.into(),
                        requester_did: author.into(),
                        node_did: node.into(),
                        session_id: request.session_id.clone().context("Undo session")?,
                    },
                    request_doc_id: physical_request.into(),
                    tool_call_doc_id: physical.into(),
                };
                editor_review::verify_model_receipt(verifier, &receipt, &demand).await?;
                ensure!(
                    receipt["status"] == "served"
                        && receipt["observation_doc_id"] == retained[0]["_docID"]
                        && receipt["outcome_signer_did"] == node,
                    "Undo image read lost source-signed selection"
                );
            }
            observations.push(json!({"requestId":request.request_id,"sessionId":request.session_id,"toolCallDocId":physical,"path":path}));
        }
    }
    ensure!(
        sessions.len() == 3,
        "Undo cases must use independent chat groups"
    );
    let goals = value["data"]["Goal"].as_array().context("Undo Goal")?;
    ensure!(
        goals.len() == 1 && goals[0]["objective"] == OBJECTIVE && goals[0]["status"] == "paused",
        "Undo lost the actual paused Goal"
    );
    let controls = value["data"]["AgentSessionGoalControl"]
        .as_array()
        .context("Undo Goal controls")?;
    ensure!(
        controls.len() == 2,
        "Undo must preserve exactly the explicit Goal start and pause"
    );
    let mut paused = false;
    for control in controls {
        let command: SessionGoalControl = serde_json::from_str(text(control, "intent_json")?)?;
        let receipt: SessionGoalControlReceipt =
            serde_json::from_str(text(control, "receipt_json")?)?;
        command.validate_shape()?;
        receipt.validate_for(&command)?;
        ensure!(
            command.node_did == node
                && command.requester_did == author
                && command.session_id == text(&goals[0], "session_id")?,
            "Undo Goal control crossed its owner"
        );
        ensure!(
            verifier
                .verify(author, &command.signing_payload()?, &command.signature)
                .await?
                && verifier
                    .verify(node, &receipt.signing_payload()?, &receipt.signature)
                    .await?,
            "Undo Goal receipt lost its source signature"
        );
        ensure!(
            matches!(receipt.outcome, GoalControlOutcome::Applied { .. }),
            "Undo Goal control was not applied"
        );
        match command.operation {
            GoalOperation::Create { objective, .. } => {
                ensure!(objective == OBJECTIVE, "Undo started an unrelated Goal")
            }
            GoalOperation::Pause {} => paused = true,
            _ => anyhow::bail!("Undo changed the paused Goal"),
        }
    }
    ensure!(paused, "Undo Goal was not explicitly paused");
    ensure!(
        provider
            .captured_chat_requests()
            .iter()
            .all(|request| !request.to_string().contains(DRAFT)),
        "Undo submitted an unsent draft"
    );
    for file in cases()[0]["files"].as_array().context("success files")? {
        ensure!(
            std::fs::read(root.join(text(file, "path")?))? == text(file, "after")?.as_bytes(),
            "replay changed recreated recorded postimage"
        );
    }
    for file in cases()[1]["files"].as_array().context("protected files")? {
        let expected = if file["path"] == "undo-disk.txt" {
            text(file, "later")?
        } else {
            text(file, "after")?
        };
        ensure!(
            std::fs::read(root.join(text(file, "path")?))? == expected.as_bytes(),
            "Undo overwrote a protected target"
        );
    }
    let partial_cases = cases();
    let partial_files = partial_cases[2]["files"]
        .as_array()
        .context("partial files")?;
    let conflict_path = text(&ui, "partialConflictPath")?;
    ensure!(
        partial_files
            .iter()
            .any(|file| file["path"] == conflict_path),
        "partial conflict target was not a fixture file"
    );
    let conflict_text = partial_files
        .iter()
        .find_map(|file| file["later"].as_str())
        .context("partial conflict text")?;
    for file in partial_files {
        let expected = if file["path"] == conflict_path {
            conflict_text
        } else {
            text(file, "before")?
        };
        ensure!(
            std::fs::read(root.join(text(file, "path")?))? == expected.as_bytes(),
            "Undo lost a partial save outcome"
        );
    }
    Ok(
        json!({"ui":ui,"physicalCaptures":observations,"sourceSignedImages":true,"independentChats":true,"pausedGoalPreserved":true}),
    )
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Undo fixture omitted {key}"))
}
