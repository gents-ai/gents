use super::{fake_llm, ChatAction};
use anyhow::{ensure, Context, Result};
use gents::ConfigAccess;
use gents_protocol::{request_lifecycle::RequestLifecycleState, row::AgentRequestRow};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub const TITLE: &str = "Recover blocked and limited goals";
pub const DRAFT: &str = "Keep this unsent goal follow-up";
pub const CAPTURED: &str = "unsent goal recovery context\n";
pub const RESUMING: &str = "Recovered goal";
pub const WRAPUP: &str = "Budget wrap-up completed.";

pub struct Provider {
    attempts: AtomicUsize,
    gate: Arc<tokio::sync::Semaphore>,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            attempts: AtomicUsize::new(0),
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
        if !is_goal_input(request) {
            return Some(ChatAction::Sse(fake_llm::completion_text_sse(TITLE)));
        }
        match self.attempts.fetch_add(1, Ordering::SeqCst) {
            0 => None,
            1 => Some(ChatAction::HttpError(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                json!({"error":{"type":"insufficient_quota", "code":"insufficient_quota", "message":"Your monthly usage limit has been reached."}}).to_string(),
            )),
            2 => Some(ChatAction::GatedSse(
                vec![(Duration::ZERO, format!("data: {}\n\n", json!({"choices":[{"delta":{"content":RESUMING},"finish_reason":null}],"usage":null})))],
                Arc::clone(&self.gate),
                fake_llm::completion_text_sse(" and finished."),
                emitted.clone(),
            )),
            3 => Some(ChatAction::Sse(fake_llm::completion_text_sse(WRAPUP))),
            attempt => panic!("unexpected goal-recovery provider attempt {attempt}"),
        }
    }

    pub fn release(&self) {
        self.gate.add_permits(1);
    }
}

pub async fn verify(
    access: &ConfigAccess,
    provider: &fake_llm::FakeLlm,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
    observation: Value,
) -> Result<Value> {
    let query = format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}}}) {{ {} }} Goal(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{ {} }} }}"#,
        gents::graphql::escape_graphql_string(node),
        gents::graphql::escape_graphql_string(session),
        gents::SIGNED_REQUEST_FIELDS,
        gents::graphql::escape_graphql_string(node),
        gents::graphql::escape_graphql_string(session),
        gents::goal::GOAL_FIELDS,
    );
    let value = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let value = access.execute(&query).await?;
            let settled = value["data"]["Goal"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1 && rows[0]["wrapup_completed"] == true)
                && value["data"]["AgentRequest"]
                    .as_array()
                    .is_some_and(|rows| {
                        rows.iter().all(|row| {
                            matches!(
                                row["lifecycle_state"].as_str(),
                                Some("completed" | "failed")
                            )
                        })
                    });
            if settled {
                return Ok::<_, anyhow::Error>(value);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("budget wrap-up did not settle through the native owner")??;
    let ui: Value =
        serde_json::from_slice(&std::fs::read(root.join("editor-goal-recovery-ui.json"))?)?;
    ensure!(
        ui["sessionId"] == session,
        "goal recovery evidence crossed sessions"
    );
    let captured = provider.captured_chat_requests();
    ensure!(
        captured
            .iter()
            .all(|request| !request.to_string().contains(DRAFT)
                && !request.to_string().contains(CAPTURED.trim())),
        "goal resume submitted the composer draft or pending capture"
    );
    let rows: Vec<AgentRequestRow> = serde_json::from_value(value["data"]["AgentRequest"].clone())?;
    ensure!(
        rows.len() == 4,
        "initial request, two explicit resumes and one budget wrap-up required: {value}"
    );
    ensure!(
        captured
            .iter()
            .filter(|request| is_goal_input(request))
            .count()
            == 4,
        "goal recovery admitted an extra provider attempt"
    );
    let goal = value["data"]["Goal"].as_array().context("goal rows")?;
    ensure!(goal.len() == 1, "goal recovery replaced the canonical goal");
    let goal = &goal[0];
    ensure!(
        goal["status"] == "budget_limited"
            && goal["token_budget"] == 1
            && goal["wrapup_requested"] == true
            && goal["wrapup_completed"] == true,
        "budget recovery did not retain its terminal native outcome: {goal}"
    );
    ensure!(
        observation["goal"]["goalId"] == goal["goal_id"],
        "goal observation crossed its native scope"
    );
    let mut failed = 0;
    let mut wrapups = 0;
    for row in &rows {
        gents::verify_request_receipt_signature(row)?;
        ensure!(
            row.requester_did.as_deref() == Some(author),
            "goal recovery changed its requester"
        );
        if let Some(facts) = row
            .input
            .as_ref()
            .and_then(|input| input.goal_continuation.as_ref())
        {
            ensure!(
                row.admission_signer_did.as_deref() == Some(node),
                "continuation bypassed native signing"
            );
            ensure!(
                row.caused_by_trigger_id.as_deref() == goal["goal_id"].as_str(),
                "continuation belongs to a different goal"
            );
            let parent = rows
                .iter()
                .find(|parent| parent.doc_id == row.caused_by_parent_request_doc_id)
                .context("physical continuation predecessor")?;
            ensure!(
                parent.request_id != row.request_id,
                "goal continuation points to itself"
            );
            wrapups += usize::from(facts.wrapup);
        } else {
            ensure!(
                row.admission_signer_did.as_deref() == Some(author),
                "initial goal request lost its client signature"
            );
        }
        if row.lifecycle_state == Some(RequestLifecycleState::Failed) {
            failed += 1;
        } else {
            ensure!(
                row.lifecycle_state == Some(RequestLifecycleState::Completed),
                "goal work still running"
            );
        }
    }
    ensure!(
        failed == 1 && wrapups == 1,
        "usage failure or bounded budget wrap-up missing"
    );
    Ok(
        json!({"nodeDid":node,"sessionId":session,"goalId":goal["goal_id"],"verifiedSignatures":rows.len(),"providerAttempts":captured.len(),"goalProviderAttempts":4,"explicitResumes":2,"usageLimited":true,"budgetWrapupCompleted":true,"observation":observation,"ui":ui}),
    )
}

fn is_goal_input(request: &Value) -> bool {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .any(|message| match &message["content"] {
            Value::String(text) => text == "Return the fixture response.",
            Value::Array(parts) => parts
                .iter()
                .any(|part| part["text"] == "Return the fixture response."),
            _ => false,
        })
}
