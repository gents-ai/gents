use super::{fake_llm, support::Attachment, ChatAction};
use anyhow::{ensure, Context, Result};
use gents::{graphql::escape_graphql_string, ConfigAccess, NodeIdentity};
use gents_protocol::{
    goal_control::{SessionGoalControl, SessionGoalControlReceipt},
    row::AgentRequestRow,
};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub const OBJECTIVE: &str = "Recover this installed Goal without replacing its input.";
pub const CAPTURE: &str = "unsaved Goal delivery context\n";
pub const DRAFT: &str = "Keep this newer Goal delivery draft.";
const COMPLETE_CALL: &str = "goal-delivery-complete";

pub struct Provider {
    started: AtomicBool,
    gate: Arc<tokio::sync::Semaphore>,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            started: AtomicBool::new(false),
            gate: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

impl Provider {
    pub fn reply(
        &self,
        request: &Value,
        emitted: &tokio::sync::watch::Sender<Option<Instant>>,
    ) -> Option<ChatAction> {
        let messages = request["messages"].as_array()?;
        if !messages
            .iter()
            .any(|row| row["role"] == "user" && row.to_string().contains(OBJECTIVE))
        {
            return None;
        }
        let completed = messages
            .iter()
            .any(|row| row["role"] == "tool" && row["tool_call_id"] == COMPLETE_CALL);
        if !completed
            && !request["tools"].as_array().is_some_and(|tools| {
                tools
                    .iter()
                    .any(|tool| tool["function"]["name"] == gents::goal::UPDATE_GOAL_TOOL_NAME)
            })
        {
            return None;
        }
        if !self.started.swap(true, Ordering::SeqCst) {
            return Some(ChatAction::GatedSse(
                vec![(
                    Duration::ZERO,
                    format!(
                        "data: {}\n\n",
                        json!({
                            "choices":[{"delta":{"content":"Original Goal work is running."},"finish_reason":null}],"usage":null,
                        })
                    ),
                )],
                Arc::clone(&self.gate),
                fake_llm::completion_text_sse(" Original Goal work finished."),
                emitted.clone(),
            ));
        }
        Some(ChatAction::Sse(if completed {
            fake_llm::completion_text_sse("Recovered Goal completed.")
        } else {
            fake_llm::tool_call_sse_with_id(
                COMPLETE_CALL,
                gents::goal::UPDATE_GOAL_TOOL_NAME,
                &json!({"status":"complete","reason":"Installed Goal delivery recovered."})
                    .to_string(),
            )
        }))
    }

    pub fn release(&self) {
        self.gate.add_permits(1);
    }
}

fn image() -> gents_protocol::request_input::RequestAttachment {
    gents_protocol::request_input::RequestAttachment {
        name: "goal.png".into(),
        media_type: "image/png".into(),
        data: super::goal_delivery::IMAGE_DATA.into(),
    }
}

pub fn verify_images(provider: &fake_llm::FakeLlm) -> Result<()> {
    let requests = provider.captured_chat_requests();
    let rounds: Vec<_> = requests
        .iter()
        .filter(|request| {
            request["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["role"] == "user"
                        && match &message["content"] {
                            Value::String(text) => text == OBJECTIVE,
                            Value::Array(parts) => {
                                parts.iter().any(|part| part["text"] == OBJECTIVE)
                            }
                            _ => false,
                        }
                })
            })
        })
        .collect();
    ensure!(!rounds.is_empty(), "Goal never reached the provider");
    let expected = format!("data:image/png;base64,{}", image().data);
    for request in rounds {
        let images: Vec<_> = request["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .filter_map(|part| part.pointer("/image_url/url").and_then(Value::as_str))
            .collect();
        ensure!(
            images == [expected.as_str()],
            "Goal round changed or duplicated its image: image count={}",
            images.len()
        );
    }
    Ok(())
}

pub fn prepare(root: &Path) -> Result<()> {
    std::fs::write(root.join("goal-delivery.rs"), "on-disk Goal context\n")?;
    std::fs::write(
        root.join("editor-goal-delivery-fixture.json"),
        serde_json::to_vec(&json!({
            "objective":OBJECTIVE,"capture":CAPTURE,"draft":DRAFT,"image":image(),
        }))?,
    )?;
    Ok(())
}

pub async fn verify(
    access: &ConfigAccess,
    client: &mut Attachment,
    id: &mut u32,
    node: &str,
    author: &str,
    identity: &dyn NodeIdentity,
    root: &Path,
) -> Result<Value> {
    let opening: Value =
        serde_json::from_slice(&std::fs::read(root.join("editor-goal-reply-loss.json"))?)?;
    let pause: Value =
        serde_json::from_slice(&std::fs::read(root.join("editor-goal-pause-loss.json"))?)?;
    let acknowledgement: Value =
        serde_json::from_slice(&std::fs::read(root.join("editor-goal-ack-loss.json"))?)?;
    ensure!(
        opening["command"] == "desktop_goal_control" && pause["command"] == "desktop_goal_control"
    );
    ensure!(acknowledgement["command"] == "desktop_goal_submission_acknowledge");
    let session = opening["sessionId"]
        .as_str()
        .context("retained Goal destination")?;
    ensure!(pause["sessionId"] == session && opening["commandId"] != pause["commandId"]);
    let node_literal = escape_graphql_string(node);
    let session_literal = escape_graphql_string(session);
    let query = format!(
        r#"{{
        AgentSessionGoalControl(filter:{{node_did:{{_eq:"{node_literal}"}},session_id:{{_eq:"{session_literal}"}}}}){{intent_json receipt_json}}
        AgentRequest(filter:{{node_did:{{_eq:"{node_literal}"}},session_id:{{_eq:"{session_literal}"}},purpose:{{_eq:"normal"}}}}){{{}}}
        Goal(filter:{{node_did:{{_eq:"{node_literal}"}},session_id:{{_eq:"{session_literal}"}}}}){{{}}}
    }}"#,
        gents::SIGNED_REQUEST_FIELDS,
        gents::goal::GOAL_FIELDS
    );
    let value = access.execute(&query).await?;
    let controls = value["data"]["AgentSessionGoalControl"]
        .as_array()
        .context("Goal commands")?;
    ensure!(
        controls.len() == 3,
        "opening, pause and explicit resume must each publish once: {value}"
    );
    let mut initial_request = None;
    for row in controls {
        let signed: SessionGoalControl =
            serde_json::from_str(row["intent_json"].as_str().context("signed command")?)?;
        let receipt: SessionGoalControlReceipt =
            serde_json::from_str(row["receipt_json"].as_str().context("signed outcome")?)?;
        receipt.validate_for(&signed)?;
        ensure!(
            identity
                .verify(author, &signed.signing_payload()?, &signed.signature)
                .await?
        );
        ensure!(
            identity
                .verify(node, &receipt.signing_payload()?, &receipt.signature)
                .await?
        );
        if signed.command_id == opening["commandId"] {
            let first = signed.initial_request.as_ref().context("opening request")?;
            ensure!(first.input.captured_context.len() == 1);
            ensure!(
                first.input.attachments == [image()],
                "saved Goal image changed after recovery"
            );
            let capture = &first.input.captured_context[0];
            ensure!(
                capture.unsaved && capture.content == CAPTURE,
                "saved input changed after reload"
            );
            initial_request = Some(first.request_id.clone());
        }
    }
    let requests: Vec<AgentRequestRow> =
        serde_json::from_value(value["data"]["AgentRequest"].clone())?;
    ensure!(
        requests.len() == 2,
        "opening and resumed continuation must remain distinct and unique: {value}"
    );
    let initial_request = initial_request.context("opening command was not recovered")?;
    ensure!(
        requests
            .iter()
            .filter(|row| row.request_id == initial_request)
            .count()
            == 1
    );
    for request in &requests {
        gents::verify_request_receipt_signature(request)?;
        ensure!(request.requester_did.as_deref() == Some(author));
        ensure!(
            request.lifecycle_state
                == Some(gents_protocol::request_lifecycle::RequestLifecycleState::Completed)
        );
        ensure!(request.content.as_deref() != Some(DRAFT));
    }
    let goals = value["data"]["Goal"].as_array().context("Goal rows")?;
    ensure!(
        goals.len() == 1 && goals[0]["status"] == "complete",
        "Goal did not complete: {value}"
    );
    for marker in [&opening, &pause] {
        let recovered = client
            .call(
                *id,
                "desktop_goal_submission_recover",
                json!({"query":{
                    "clientDid":author,"nodeDid":node,"commandId":marker["commandId"],
                }}),
            )
            .await;
        *id += 1;
        ensure!(recovered["kind"] == "prepared" && recovered["acknowledged"] == true);
        ensure!(recovered["outcome"]["kind"] == "applied");
    }
    Ok(json!({"opening":opening,"pause":pause,"acknowledgement":acknowledgement,"records":value}))
}
