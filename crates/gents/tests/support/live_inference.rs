use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents::defra_node::Node as EmbeddedNode;
use gents::documents::{Agent, InferenceBackend, InferenceProfile, InferenceSampling, Node};
use gents::store::config_client::DesiredStateApplyPlan;
use gents::store::graphql::escape_graphql_string;
use gents::{
    default_inference_profile_id_for_agent, ensure_node, Collection, DocumentRuntimeOptions, Gents,
    NodeIdentity, ToolCeiling,
};
use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
use gents_protocol::output::{OutputSegment, TranscriptMessage};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use crate::support::interrupt::BootedAgent;
use crate::support::{first_optional_row, TestDb};

#[path = "inference_target.rs"]
pub mod inference_target;
pub use inference_target::InferenceTarget;

pub fn live_target() -> InferenceTarget {
    InferenceTarget::selected().unwrap_or_else(|error| panic!("{error:#}"))
}

pub async fn bind_target(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    target: &InferenceTarget,
) -> (String, String) {
    let node_did = identity.did().to_string();
    let mut node_doc = ensure_node(node, &node_did).await.expect("ensure node");
    let binding = gents::store::identity::provision_agent_identity(identity).unwrap();
    let agent_did = binding.agent_did;
    let profile_id = default_inference_profile_id_for_agent(&agent_did);
    node_doc.default_agent_did = Some(agent_did.clone());
    let backend = target.backend(&node_did);
    let profile = InferenceProfile {
        profile_id: profile_id.clone(),
        ..target.profile(&node_did)
    };
    let agent = Agent {
        agent_did: agent_did.clone(),
        creation_nonce: binding.creation_nonce,
        node_did: node_did.clone(),
        display_name: Some("Live default agent".to_string()),
        description: None,
        context_id: None,
        inference_profile_id: profile_id,
        enabled: true,
        tags: Vec::new(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    apply_live_backend_documents(
        node,
        node_doc,
        backend,
        profile,
        target.sampling(&node_did),
        agent,
    )
    .await;
    (node_did, agent_did)
}

async fn apply_live_backend_documents(
    node: &Arc<EmbeddedNode>,
    node_doc: Node,
    backend: InferenceBackend,
    profile: InferenceProfile,
    sampling: Option<InferenceSampling>,
    agent: Agent,
) {
    use gents::store::config_client::{apply_desired_state_plan, DesiredStateApplyDocument};
    let plan = DesiredStateApplyPlan::new(
        [
            (Collection::Node, serde_json::to_value(node_doc)),
            (Collection::InferenceBackend, serde_json::to_value(backend)),
            (Collection::InferenceProfile, serde_json::to_value(profile)),
            (Collection::Agent, serde_json::to_value(agent)),
        ]
        .into_iter()
        .chain(sampling.map(|sampling| {
            (
                Collection::InferenceSampling,
                serde_json::to_value(sampling),
            )
        }))
        .map(|(collection, value)| {
            let value = value.expect("serialize live configuration document");
            DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            }
        })
        .collect(),
    )
    .expect("build live backend plan");
    gents::ConfigAccess::transact_local_write(node, None, "test.bind_live_backend", |txn| {
        let plan = &plan;
        Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
    })
    .await
    .expect("upsert live backend");
}

pub async fn boot_live_agent(db: &TestDb, identity: Arc<dyn NodeIdentity>) -> Result<BootedAgent> {
    boot_live_agent_with_ceiling(db, identity, ToolCeiling::meta_only()).await
}

pub async fn boot_live_agent_with_ceiling(
    db: &TestDb,
    identity: Arc<dyn NodeIdentity>,
    tool_ceiling: ToolCeiling,
) -> Result<BootedAgent> {
    Ok(boot_live_agent_with_options(
        db,
        identity,
        DocumentRuntimeOptions {
            tool_ceiling,
            ..Default::default()
        },
    )
    .await?
    .0)
}

pub async fn boot_live_agent_with_options(
    db: &TestDb,
    identity: Arc<dyn NodeIdentity>,
    options: DocumentRuntimeOptions,
) -> Result<(BootedAgent, Gents)> {
    let (running, agent) =
        gents::eval::runner::embedded::boot_runtime(&db.home, identity, options).await?;
    Ok((
        BootedAgent::new(running.shutdown, running.handle, running.node_did),
        agent,
    ))
}

fn is_terminal(state: &str) -> bool {
    RequestLifecycleState::is_terminal_str(Some(state))
}

async fn fetch_request_lifecycle(node: &EmbeddedNode, request_id: &str) -> Option<String> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                lifecycle_state
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct Row {
        lifecycle_state: Option<String>,
    }
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "request observation failed: {:?}",
        resp.errors
    );
    let rows = resp.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert!(
        rows.len() <= 1,
        "live test request label is ambiguous: {request_id}"
    );
    first_optional_row::<Row>(&resp, "AgentRequest").and_then(|r| r.lifecycle_state)
}

pub async fn wait_for_request_terminal(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        if let Some(state) = fetch_request_lifecycle(node, request_id).await {
            last = state.clone();
            if is_terminal(&state) {
                return state;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let escaped = escape_graphql_string(request_id);
            let evidence = node
                .execute(&format!(
                    r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                    _docID lifecycle_state execution_generation execution_lease_expires_at
                    terminal_output failure_reason
                }}
                InferenceCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 10) {{
                    call_id call_state failure_reason started_at ended_at
                }}
                AgentToolCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 10) {{
                    _docID tool_name lifecycle_state started_at completed_at
                }}
            }}"#
                ))
                .await;
            panic!("timed out waiting for request {request_id} to terminalize; last={last}; evidence={:?}; errors={:?}", evidence.data, evidence.errors);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub async fn terminal_assistant_answer(node: &EmbeddedNode, request_id: &str) -> String {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                _docID node_did requester_did session_id terminal_output
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct RequestRow {
        #[serde(rename = "_docID")]
        doc_id: String,
        node_did: String,
        requester_did: Option<String>,
        session_id: Option<String>,
        terminal_output: Option<gents_protocol::output::TerminalOutput>,
    }
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "canonical live answer request lookup failed: {:?}",
        resp.errors
    );
    let row = first_optional_row::<RequestRow>(&resp, "AgentRequest");
    let Some(row) = row else { return String::new() };
    let session_id = match row.session_id {
        Some(s) if !s.is_empty() => s,
        _ => return String::new(),
    };
    let escaped_session = escape_graphql_string(&session_id);
    let requester_filter = row
        .requester_did
        .as_deref()
        .map(|did| {
            format!(
                r#", requester_did: {{ _eq: "{}" }}"#,
                escape_graphql_string(did)
            )
        })
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
    let Some(gents_protocol::output::TerminalOutput::Message { message_doc_id }) =
        row.terminal_output
    else {
        return String::new();
    };
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ _docID: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{}" }}, session_id: {{ _eq: "{escaped_session}" }}, node_did: {{ _eq: "{}" }}, role: {{ _eq: "assistant" }}{requester_filter} }}, limit: 2
            ) {{ message_key session_id node_did requester_did request_doc_id publication outcome sequence role native_id blocks created_at }}
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }}{requester_filter} }}) {{ _docID node_did requester_did session_id request_doc_id source ordinal writer runs payload close created_at }}
        }}"#,
        escape_graphql_string(&message_doc_id),
        escape_graphql_string(&row.doc_id),
        escape_graphql_string(&row.node_did),
        escape_graphql_string(&row.doc_id),
        escape_graphql_string(&row.node_did)
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "canonical live answer header lookup failed: {:?}",
        resp.errors
    );
    let headers = resp
        .data
        .as_ref()
        .and_then(|data| data["AgentMessage"].as_array())
        .expect("canonical live answer query omitted headers");
    assert_eq!(
        headers.len(),
        1,
        "terminal output did not select one physical assistant header"
    );
    let header = serde_json::from_value::<TranscriptMessage>(headers[0].clone())
        .expect("decode selected canonical terminal header");
    let observed = resp
        .data
        .as_ref()
        .and_then(|data| data["AgentOutputSegment"].as_array())
        .expect("canonical live answer query omitted segments")
        .iter()
        .map(|value| {
            let doc_id = value["_docID"]
                .as_str()
                .expect("canonical segment omitted physical ID")
                .to_owned();
            let mut segment = value.clone();
            segment
                .as_object_mut()
                .expect("canonical segment row object")
                .remove("_docID");
            (
                doc_id,
                serde_json::from_value::<OutputSegment>(segment)
                    .expect("decode canonical output segment"),
            )
        })
        .collect::<Vec<_>>();
    let observations = observed
        .iter()
        .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
        .collect::<Vec<_>>();
    let message = reconstruct_message(&observations, &[], &[], &header)
        .expect("reconstruct selected canonical terminal message");
    gents_protocol::transcript::present_message(&message).body_markdown
}

pub async fn wait_for_assistant_answer(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let answer = terminal_assistant_answer(node, request_id).await;
        if !answer.trim().is_empty() {
            return answer;
        }
        if tokio::time::Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
