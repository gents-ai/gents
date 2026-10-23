use super::{fake_llm, ChatAction};
use anyhow::{Context, Result};
use gents::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::{load_agent_context_and_tools_in_txn, AgentTools};
use gents::{Collection, ConfigAccess};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

pub const PROMPT: &str = "Start the two related-work fixtures.";
pub const REPLY: &str = "Both related sessions are running.";
const WORKERS: [(&str, &str); 2] = [
    ("Inspect layout", "RELATED_LAYOUT_WORK"),
    ("Check history", "RELATED_HISTORY_WORK"),
];

pub struct Provider {
    gates: [Arc<Semaphore>; 2],
}
impl Provider {
    pub fn new() -> Self {
        Self {
            gates: std::array::from_fn(|_| Arc::new(Semaphore::new(0))),
        }
    }
    pub fn reply(&self, request: &Value) -> Option<ChatAction> {
        let messages = request["messages"].as_array()?;
        let has_user = |expected: &str| {
            messages.iter().any(|message| {
                message["role"] == "user"
                    && match &message["content"] {
                        Value::String(text) => text.ends_with(expected),
                        Value::Array(parts) => parts.iter().any(|part| {
                            part["text"]
                                .as_str()
                                .is_some_and(|text| text.ends_with(expected))
                        }),
                        _ => false,
                    }
            })
        };
        for (index, (_, prompt)) in WORKERS.iter().enumerate() {
            if has_user(prompt) {
                return Some(ChatAction::WaitThenSse(
                    self.gates[index].clone(),
                    fake_llm::completion_text_sse(&format!("{} complete.", WORKERS[index].0)),
                ));
            }
        }
        if !has_user(PROMPT) {
            return None;
        }
        for (index, (title, prompt)) in WORKERS.iter().enumerate() {
            let call = format!("related-worker-{index}");
            if !messages
                .iter()
                .any(|message| message["role"] == "tool" && message["tool_call_id"] == call)
            {
                return Some(ChatAction::Sse(fake_llm::tool_call_sse_with_id(
                    &call,
                    "agent_new",
                    &json!({"agent":format!("related-worker-{index}"), "prompt":prompt, "title":title}).to_string(),
                )));
            }
        }
        Some(ChatAction::Sse(fake_llm::completion_text_sse(REPLY)))
    }
    pub async fn release_when_requested(&self, root: &Path) -> Result<()> {
        for (index, gate) in self.gates.iter().enumerate() {
            tokio::time::timeout(Duration::from_secs(90), async {
                while !root.join(format!("release-related-{index}")).is_file() {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .context("editor did not release the related worker")?;
            gate.add_permits(1);
        }
        Ok(())
    }
}

pub async fn configure(access: &ConfigAccess, node: &str, agent: &str) -> Result<()> {
    access.transact_write("test.editor.related_work.configure", |txn| {
        Box::pin(async move {
            let (_, tools) = load_agent_context_and_tools_in_txn(txn, node, agent).await?;
            let mut tools = tools.context("fixture agent tools")?;
            let (_, base) = gents::config_client::read_desired_state_record_in_txn(txn, Collection::Agent, node, agent).await?.context("fixture agent")?;
            let mut documents = Vec::new();
            let targets: Vec<_> = (0..2).map(|index| format!("related-worker-{index}")).collect();
            for target in &targets {
                let mut worker = base.clone();
                worker["agent_id"] = json!(format!("{agent}-{target}"));
                documents.push((Collection::Agent, worker));
                documents.push((Collection::AgentTarget, json!({"node_did":node,"target_id":target,"name":target,"target_node_did":node,"agent_id":format!("{agent}-{target}")})));
            }
            tools.agents = Some(AgentTools { enabled: Some(true), target_ids: targets });
            documents.push((Collection::Tools, serde_json::to_value(tools)?));
            let plan = DesiredStateApplyPlan::new(documents.into_iter().map(|(collection, value)| DesiredStateApplyDocument {collection, add:value.clone(), update:value}).collect())?;
            apply_desired_state_plan(txn, &plan).await?;
            Ok(())
        })
    }).await
}
pub async fn verify(
    access: &ConfigAccess,
    node: &str,
    session: &str,
    author: &str,
    root: &Path,
) -> Result<Value> {
    use anyhow::ensure;
    use gents::graphql::escape_graphql_string as escape;
    use gents_protocol::row::AgentRequestRow;
    let response = access
        .execute(&format!(
            "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ {} }} }}",
            escape(node),
            gents::SIGNED_REQUEST_FIELDS
        ))
        .await?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    let parents: Vec<_> = rows
        .iter()
        .filter(|row| row.content.as_deref() == Some(PROMPT))
        .collect();
    ensure!(
        parents.len() == 1,
        "related-work input must be admitted once"
    );
    let parent = parents[0];
    gents::verify_request_receipt_signature(parent)?;
    ensure!(
        parent.session_id.as_deref() == Some(session)
            && parent.requester_did.as_deref() == Some(author),
        "parent input scope changed"
    );
    let children: Vec<_> = rows
        .iter()
        .filter(|row| {
            WORKERS.iter().any(|(_, prompt)| {
                row.content
                    .as_deref()
                    .is_some_and(|text| text.ends_with(prompt))
            })
        })
        .collect();
    ensure!(
        children.len() == 2,
        "two tool calls must admit exactly two related requests: {children:?}"
    );
    let mut sessions = rapidhash::RapidHashSet::default();
    for child in &children {
        gents::verify_request_receipt_signature(child)?;
        ensure!(
            child.caused_by_parent_request_doc_id == parent.doc_id
                && child.caused_by_parent_request_id.as_deref() == Some(parent.request_id.as_str())
                && child.requester_did.as_deref() == Some(node)
                && child.lifecycle_state
                    == Some(gents_protocol::request_lifecycle::RequestLifecycleState::Completed),
            "related request lost its signed physical ancestry or completion: {child:?}"
        );
        ensure!(
            child.caused_by_parent_tool_call_doc_id.is_some(),
            "related request has no physical tool cause"
        );
        ensure!(
            child.session_id != parent.session_id,
            "related work reused the parent session"
        );
        ensure!(
            sessions.insert(child.session_id.clone()),
            "two starts reused one session"
        );
    }
    let ui: Value =
        serde_json::from_slice(&std::fs::read(root.join("editor-related-work-ui.json"))?)?;
    ensure!(
        ui["concurrentWorkers"] == 2
            && ui["independentlyObservedCompletion"] == true
            && ui["reloadPreservedRowsAndMarkers"] == true
            && ui["separatePanelVerified"] == true
            && ui["parentDraftPreserved"] == true,
        "installed related-work evidence is incomplete"
    );
    Ok(
        json!({"parentRequest":parent.doc_id,"relatedSessions":sessions,"signedAncestryVerified":true,"ui":ui}),
    )
}
