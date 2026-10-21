use super::{fake_llm, ChatAction};
use anyhow::{ensure, Context, Result};
use gents::ConfigAccess;
use gents_protocol::{request_lifecycle::RequestLifecycleState, row::AgentRequestRow};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const OBJECTIVES: [&str; 2] = ["Verify active goal removal", "Verify paused goal removal"];
pub const DRAFT: &str = "Keep this unsent removal draft";
pub const QUEUED: &str = "Run this queued follow-up after goal removal";
pub const OBSERVER_DRAFT: &str = "Keep this observer conversation draft";
pub const OBSERVER_EDIT: &str = "Keep this unsaved observer goal edit";

pub struct Provider {
    gates: [Arc<tokio::sync::Semaphore>; 2],
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            gates: std::array::from_fn(|_| Arc::new(tokio::sync::Semaphore::new(0))),
        }
    }
}

impl Provider {
    pub fn reply(
        &self,
        request: &Value,
        emitted: &tokio::sync::watch::Sender<Option<Instant>>,
    ) -> Option<ChatAction> {
        if is_input(request, QUEUED) {
            return Some(ChatAction::Sse(fake_llm::completion_text_sse(
                "Queued follow-up finished.",
            )));
        }
        for (index, objective) in OBJECTIVES.iter().enumerate() {
            if is_input(request, objective) {
                return Some(ChatAction::GatedSse(
                    vec![(
                        Duration::ZERO,
                        format!(
                            "data: {}\n\n",
                            json!({"choices":[{"delta":{"content":"Goal work is running."},"finish_reason":null}],"usage":null})
                        ),
                    )],
                    Arc::clone(&self.gates[index]),
                    fake_llm::completion_text_sse(" Current work finished."),
                    emitted.clone(),
                ));
            }
        }
        (!is_input(request, "Return the fixture response."))
            .then(|| ChatAction::Sse(fake_llm::completion_text_sse("Goal removal checks")))
    }

    pub fn release(&self, index: usize) {
        self.gates[index].add_permits(1);
    }
}

pub async fn verify(
    access: &ConfigAccess,
    provider: &fake_llm::FakeLlm,
    root: &Path,
    node: &str,
    author: &str,
) -> Result<Value> {
    let node_literal = gents::graphql::escape_graphql_string(node);
    let query = format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{node_literal}"}}, purpose: {{_eq: "normal"}}}}) {{ {} interrupt_requested_at }} Goal(filter: {{node_did: {{_eq: "{node_literal}"}}}}) {{ {} }} AgentSessionGoalControl(filter: {{node_did: {{_eq: "{node_literal}"}}}}) {{ intent_json }} }}"#,
        gents::SIGNED_REQUEST_FIELDS,
        gents::goal::GOAL_FIELDS,
    );
    let value = access.execute(&query).await?;
    let goals = value["data"]["Goal"].as_array().context("goal rows")?;
    ensure!(
        goals.len() == 2,
        "removal must retain both canonical goals: {value}"
    );
    for objective in OBJECTIVES {
        let matching: Vec<_> = goals
            .iter()
            .filter(|row| row["objective"] == objective)
            .collect();
        ensure!(
            matching.len() == 1 && matching[0]["status"] == "stopped",
            "removed goal lost its durable outcome: {value}"
        );
        ensure!(
            matching[0]["completion_evidence"].is_null(),
            "removal claimed completion evidence"
        );
    }
    let rows: Vec<AgentRequestRow> = serde_json::from_value(value["data"]["AgentRequest"].clone())?;
    ensure!(
        rows.len() == 4,
        "initial chat, two goals and one queued follow-up required: {value}"
    );
    for row in &rows {
        gents::verify_request_receipt_signature(row)?;
        ensure!(
            row.requester_did.as_deref() == Some(author)
                && row.admission_signer_did.as_deref() == Some(author),
            "removal changed the request signer"
        );
        ensure!(
            row.lifecycle_state == Some(RequestLifecycleState::Completed)
                && row.interrupt_requested_at.is_none(),
            "removal cancelled owned work: {value}"
        );
        ensure!(
            row.input
                .as_ref()
                .and_then(|input| input.goal_continuation.as_ref())
                .is_none(),
            "removed goal admitted autonomous continuation"
        );
    }
    let captured = provider.captured_chat_requests();
    ensure!(
        captured
            .iter()
            .all(|request| ![DRAFT, OBSERVER_DRAFT, OBSERVER_EDIT]
                .iter()
                .any(|draft| request.to_string().contains(draft))),
        "removal submitted the unsent composer draft"
    );
    ensure!(
        captured
            .iter()
            .filter(|request| is_input(request, QUEUED))
            .count()
            == 1,
        "queued input was cancelled or replayed"
    );
    let commands: Vec<Value> = value["data"]["AgentSessionGoalControl"]
        .as_array()
        .context("goal control rows")?
        .iter()
        .map(|row| {
            serde_json::from_str(row["intent_json"].as_str().context("goal control intent")?)
                .map_err(anyhow::Error::from)
        })
        .collect::<Result<_>>()?;
    ensure!(
        commands.len() == 5,
        "observer window published an unexpected goal control: {commands:?}"
    );
    for (kind, count) in [("create", 2), ("pause", 1), ("stop", 2)] {
        ensure!(
            commands
                .iter()
                .filter(|command| command["operation"]["kind"] == kind)
                .count()
                == count,
            "unexpected {kind} controls: {commands:?}"
        );
    }
    let ui: Value =
        serde_json::from_slice(&std::fs::read(root.join("editor-goal-removal-ui.json"))?)?;
    for field in [
        "twoWindowsObserved",
        "staleGoalEditPreserved",
        "observerDraftPreserved",
    ] {
        ensure!(ui[field] == true, "editor removal evidence omitted {field}");
    }
    Ok(
        json!({"nodeDid":node,"goals":goals,"verifiedRequestSignatures":rows.len(),"noAutonomousContinuation":true,"noInterrupt":true,"ui":ui}),
    )
}

fn is_input(request: &Value, expected: &str) -> bool {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .any(|message| match &message["content"] {
            Value::String(text) => text == expected,
            Value::Array(parts) => parts.iter().any(|part| part["text"] == expected),
            _ => false,
        })
}
