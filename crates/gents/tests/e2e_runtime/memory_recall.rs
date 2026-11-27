use std::collections::BTreeMap;

use gents::test_support::running_runtime::{AgentRuntimeHome, BootedRuntime};
use gents::DocumentRuntimeOptions;
use gents_inference::test_support::scripted_backend::{ScriptedBackend, StreamScript};
use gents_loop::tool::{ToolCallEnvelope, ToolDyn};
use gents_loop::tool_call_lifecycle::runtime::{
    CurrentToolRuntimeContext, ToolRequestIdentity, ToolScopeEntry,
};
use gents_protocol::node_readiness::{AgentReadinessState, NodeReadinessProcessState};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::support::interrupt::wait_for_request_terminal_state;

const MODEL: &str = "default";
const FIRST_SESSION: &str = "session-memory-recall-first";
const SECOND_SESSION: &str = "session-memory-recall-second";
const ROOT_CAUSE: &str = "the startup read races the migration";

fn runtime_for(agent_did: &str, request_id: &str) -> CurrentToolRuntimeContext {
    let entry = ToolScopeEntry {
        identity: Some(ToolRequestIdentity::new(
            None,
            None,
            Some(agent_did.to_owned()),
            Some(request_id.to_owned()),
        )),
        deadline_at: None,
        cancellation_token: CancellationToken::new(),
        workspace_cwd: None,
        live_output: None,
        session_id: Some(FIRST_SESSION.to_owned()),
        correlation: Some("memory-recall-runtime-call".to_owned()),
        source_fields: BTreeMap::new(),
        background: false,
    };
    CurrentToolRuntimeContext::entered(None, entry)
}

async fn call(tool: &dyn ToolDyn, args: Value, runtime: CurrentToolRuntimeContext) -> Value {
    let envelope = ToolCallEnvelope::new(Some(runtime));
    let text = tool
        .call(args.to_string(), &envelope)
        .await
        .expect("the memory tool succeeds");
    serde_json::from_str(&text).expect("the memory tool returns JSON")
}

#[tokio::test]
async fn the_provider_input_carries_the_experience_block() {
    let backend = ScriptedBackend::start(
        MODEL,
        vec![
            StreamScript::completes("memory-recall-first", ["noted"]),
            StreamScript::completes("memory-recall-second", ["again"]),
        ],
    )
    .expect("mock backend");
    let home = AgentRuntimeHome::start(
        "memory-recall-runtime",
        None,
        backend.endpoint(),
        2,
        gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
    )
    .await;
    let agent_did = home.agent.did().to_owned();
    let runtime = BootedRuntime::boot(&home, DocumentRuntimeOptions::default()).await;
    runtime
        .readiness_when("the fixture agent holds a ready executor", |snapshot| {
            snapshot.process_state == NodeReadinessProcessState::Ready
                && snapshot.agents.iter().any(|entry| {
                    entry.agent_did == agent_did && entry.state == AgentReadinessState::Ready
                })
        })
        .await;

    let first = home
        .submit_request(
            &agent_did,
            "request-memory-recall-first",
            FIRST_SESSION,
            "memory-recall-first",
        )
        .await;
    assert_eq!(
        wait_for_request_terminal_state(&home.home.user_access, &first).await,
        RequestLifecycleState::Completed
    );

    let tool = gents_tools::memory::build_memory_tool(
        home.home.node.clone(),
        home.home.node_did().to_owned(),
        agent_did.clone(),
        home.agent.as_ref(),
    )
    .await
    .expect("build the memory tool");
    let context = runtime_for(&agent_did, "request-memory-recall-first");
    let problem = call(
        tool.as_ref(),
        json!({"action": "problem", "name": ROOT_CAUSE}),
        context.clone(),
    )
    .await;
    let problem_key = problem["problem_key"]
        .as_str()
        .expect("problem key")
        .to_owned();
    call(
        tool.as_ref(),
        json!({
            "action": "attempt",
            "problem_key": problem_key,
            "what": "wait for the migration before the first read",
            "predicted": ["the failure is gone"]
        }),
        context,
    )
    .await;

    let second = home
        .submit_request(
            &agent_did,
            "request-memory-recall-second",
            SECOND_SESSION,
            &format!("{ROOT_CAUSE} memory-recall-second"),
        )
        .await;
    assert_eq!(
        wait_for_request_terminal_state(&home.home.user_access, &second).await,
        RequestLifecycleState::Completed
    );
    backend.wait_for_request("memory-recall-second").await;

    let bodies: Vec<String> = backend
        .observed_turn_completion_bodies()
        .iter()
        .map(|body| serde_json::to_string(body).expect("the body serializes"))
        .collect();
    let second_body = bodies
        .iter()
        .find(|body| body.contains("memory-recall-second"))
        .expect("the second turn reached the provider");
    assert!(
        second_body.contains("<experience>"),
        "the provider input carries the experience block: {second_body}"
    );
    assert!(
        second_body.contains(&problem_key),
        "the experience block names the recorded problem: {second_body}"
    );
    let first_body = bodies
        .iter()
        .find(|body| body.contains("memory-recall-first"))
        .expect("the first turn reached the provider");
    assert!(
        !first_body.contains("<experience>"),
        "the first turn ran before any problem was recorded: {first_body}"
    );

    runtime.shutdown().await.expect("the runtime shuts down");
}
