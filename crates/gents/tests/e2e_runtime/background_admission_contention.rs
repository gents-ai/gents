use gents::runtime::agent::DocumentRuntimeOptions;
use gents::test_support::running_runtime::{AgentRuntimeHome, BootedRuntime};
use gents_documents::{BashMode, BashTools, HostTools, Tools};
use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamChunk, StreamPlan, StreamResponse,
};
use gents_lean_vocab::tla::replay::{Red, Trace};
use gents_protocol::job::BackgroundLimit;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::tool_call::ToolCallState;
use gents_session::tool_call::query::result::{load_tool_call_read, render_tool_result};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_store::test_support::rows_when;
use gents_toolcalls::test_support::background_hook::admission_receipt_matches;
use gents_toolcalls::test_support::held_process::HeldProcess;
use gents_tools::surface::ToolCeiling;
use serde_json::{json, Value};

fn read_line(input: &HeldProcess) -> Value {
    json!({"command":"head","args":["-n","1",input.input()]})
}

fn rows(data: &Value) -> &[Value] {
    let rows = data["AgentToolCall"].as_array().unwrap();
    for row in rows.iter().filter(|r| r["tool_name"] == "bash") {
        let state =
            ToolCallState::from_persisted(row["lifecycle_state"].as_str().unwrap()).unwrap();
        assert!(
            !state.is_terminal() || state == ToolCallState::Completed,
            "held bash call ended unexpectedly: {row}"
        );
    }
    rows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_admission_contention_concurrent_turn() {
    let attempts = gents_store::config_client::TRANSACT_CONFLICT_MAX_RETRIES + 1;
    let trace = Trace::load(
        include_str!(
            "../../proofs/tla/generated/traces/MCBackgroundAdmissionContentionConcurrent.json"
        ),
        "BackgroundAdmissionContention",
        &[("StartAttempts", attempts), ("RenewAttempts", attempts)],
    );
    let n = trace.constants["N"].as_u64().unwrap() as usize;
    let limit = BackgroundLimit::resolve(trace.constants["Max"].as_i64());
    assert_eq!(trace.constants["Max"], limit.get());
    let root = tempfile::tempdir().unwrap();
    let mut inputs = Vec::new();
    for actor in 1..=n {
        let directory = root.path().join(actor.to_string());
        tokio::fs::create_dir(&directory).await.unwrap();
        inputs.push(HeldProcess::new(&directory).await);
    }
    let mut chunks = Vec::new();
    let mut offered = Vec::new();
    let mut releases = Vec::new();
    for step in &trace.steps {
        let i = step.actor.as_ref().unwrap().as_u64().unwrap() as usize - 1;
        match step.action.as_str() {
            "Begin" if !offered.contains(&i) => {
                chunks.push(StreamChunk::tool_call(
                    format!("spawn-{i}"),
                    "spawn_process",
                    json!({"tool_name":"bash","args": read_line(&inputs[i])}).to_string(),
                ));
                offered.push(i);
            }
            "ChildCompletes" => {
                let dir = root.path().join(format!("checkpoint-{i}"));
                tokio::fs::create_dir(&dir).await.unwrap();
                let barrier = HeldProcess::new(&dir).await;
                chunks.push(StreamChunk::tool_call(
                    format!("checkpoint-{i}"),
                    "bash",
                    read_line(&barrier).to_string(),
                ));
                releases.push((
                    i,
                    offered.iter().position(|actor| *actor == i).unwrap(),
                    barrier,
                ));
            }
            "Begin" | "CommitStart" | "CommitAdmission" | "CommitReceipt" | "Abort" => {}
            other => panic!("unmapped sibling action {other}"),
        }
    }
    let count = offered.len();
    let marker = "overlapping spawn requests";
    let backend = ScriptedBackend::start_with_plans(
        "default",
        vec![StreamPlan::new(
            marker,
            vec![
                StreamResponse::streams(marker, chunks),
                StreamResponse::completes(marker, ["settled"]).after("parents-settled"),
            ],
        )],
    )
    .unwrap();
    let home = AgentRuntimeHome::start_with_wire_api(
        "concurrent-admission",
        None,
        backend.endpoint(),
        1,
        gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
        Some(gents_loop::openai_wire::OpenAiWireApi::Responses),
    )
    .await;
    crate::support::fixtures::configure_agent_tools_as(
        &home.home.user_access,
        home.home.node_did(),
        &home.binding.agent_did,
        None,
        Tools {
            tools_id: "admission-tools".into(),
            node_did: home.home.node_did().into(),
            host: Some(HostTools {
                root: Some(root.path().display().to_string()),
                max_background_tool_calls: trace.constants["Max"].as_i64(),
                bash: Some(BashTools {
                    mode: BashMode::ReadOnly,
                    background_enabled: true,
                    read_only_commands: Some(vec!["head".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let runtime = BootedRuntime::boot(
        &home,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readonly_at(root.path()),
            ..Default::default()
        },
    )
    .await;
    let readiness = runtime.readiness().await.unwrap();
    let entry = readiness
        .agents
        .iter()
        .find(|entry| entry.agent_did == home.binding.agent_did)
        .unwrap();
    assert_eq!(
        entry.state,
        gents_protocol::node_readiness::AgentReadinessState::Ready,
        "{entry:?}"
    );
    let session = "admission-session";
    let doc = home
        .submit_request(
            &home.binding.agent_did,
            "admission-request",
            session,
            marker,
        )
        .await;
    let access = home.agent_access();
    let escaped_doc = escape_graphql_string(&doc);
    let query = format!(
        r#"{{ AgentToolCall(filter: {{request_doc_id: {{_eq: "{escaped_doc}"}}}}) {{ _docID tool_name tool_call_id lifecycle_state await_mode started_at completed_at spawned_by_tool_call_doc_id tool_failure_class denial_reason }} }}"#
    );
    for (index, (actor, ordinal, mut barrier)) in releases.into_iter().enumerate() {
        let checkpoint = rows_when(&access, "AgentToolCall", &query, |data| {
            rows(data)
                .iter()
                .filter(|r| {
                    r["tool_name"] == "bash"
                        && r["await_mode"] == "foreground"
                        && !r["started_at"].is_null()
                })
                .count()
                > index
        })
        .await;
        let mut parents: Vec<_> = rows(&checkpoint)
            .iter()
            .filter(|r| r["tool_name"] == "spawn_process" && !r["started_at"].is_null())
            .collect();
        parents.sort_by_key(|r| r["started_at"].as_str().unwrap());
        let parent = parents.get(ordinal).unwrap_or_else(|| {
            panic!(
                "actor {} had no admission at the foreground call: {checkpoint}",
                actor + 1
            )
        });
        let child = rows(&checkpoint)
            .iter()
            .find(|r| r["spawned_by_tool_call_doc_id"] == parent["_docID"])
            .unwrap_or_else(|| panic!("actor {} had no accepted child: {checkpoint}", actor + 1));
        inputs[actor].release_line().await;
        rows_when(&access, "AgentToolCall", &query, |data| {
            rows(data)
                .iter()
                .any(|r| r["_docID"] == child["_docID"] && r["lifecycle_state"] == "completed")
        })
        .await;
        barrier.release_line().await;
        rows_when(&access, "AgentToolCall", &query, |data| {
            rows(data)
                .iter()
                .filter(|r| {
                    r["tool_name"] == "bash"
                        && r["await_mode"] == "foreground"
                        && r["lifecycle_state"] == "completed"
                })
                .count()
                > index
        })
        .await;
    }
    let observed = rows_when(&access, "AgentToolCall", &query, |data| {
        rows(data)
            .iter()
            .filter(|r| r["tool_name"] == "spawn_process" && !r["completed_at"].is_null())
            .count()
            == count
    })
    .await;
    let mut parents: Vec<_> = rows(&observed)
        .iter()
        .filter(|r| r["tool_name"] == "spawn_process")
        .collect();
    parents.sort_by_key(|r| r["started_at"].as_str().unwrap());
    let sequential = parents
        .windows(2)
        .all(|p| p[0]["completed_at"].as_str().unwrap() <= p[1]["started_at"].as_str().unwrap());
    let mut settled = true;
    for parent in parents {
        let admission = load_tool_call_read(
            &access,
            parent["_docID"].as_str().unwrap(),
            runtime.node_did(),
            session,
            Some(home.home.user.did()),
        )
        .await
        .unwrap();
        let result: Value =
            serde_json::from_str(&render_tool_result(admission.result.as_ref().unwrap()).unwrap())
                .unwrap();
        settled &= admission_receipt_matches(parent, rows(&observed), &result, limit);
    }
    trace.check(Red::Defect, &[("NoConflictLoss", settled && sequential)]);
    for input in &mut inputs {
        input.release_line().await;
    }
    rows_when(&access, "AgentToolCall", &query, |data| {
        rows(data)
            .iter()
            .filter(|r| r["await_mode"] == "background")
            .all(|r| r["lifecycle_state"] == "completed")
    })
    .await;
    backend.release("parents-settled");
    assert_eq!(
        crate::support::interrupt::wait_for_request_terminal_state(&access, &doc).await,
        RequestLifecycleState::Completed
    );
    runtime.shutdown().await.unwrap();
    backend.shutdown().await;
}
