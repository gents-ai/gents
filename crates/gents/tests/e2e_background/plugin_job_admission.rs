use std::path::Path;
use std::time::Duration;

use gents::DocumentRuntimeOptions;
use gents_inference::llm::message::{Message, Text, ToolResultContent, UserContent};
use gents_toolcalls::background::job::job::{
    JobChildState, JobReadError, JobSpawn, JobState, JobStepOutput,
};
use gents_toolcalls::background::job::view::{load_job_view, JobKey};
use gents_tools::surface::ToolCeiling;
use serde_json::{json, Value};

use crate::plugin_job_support::{
    effects, guest_pin, install_job_fixture, job_call, job_settings, spawn, JobHarness, PlanEntry,
    MODEL,
};
use crate::support::accepted_turn::{
    boot_managed_accepted_turn_with_backend_capacity, AcceptedTurnSpec,
};
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use crate::support::{test_db, TestDb};

const CAPACITY_EXCEEDED: &str = "background_tool_budget_exceeded";

fn entry(when: Value, output: JobStepOutput) -> PlanEntry {
    PlanEntry {
        when,
        output,
        control: None,
    }
}

/// Step 0 pins the job and spawns `spawn`; the holding entry keeps the job
/// alive while its child is live.
fn plan(handle: &str, spawn: JobSpawn) -> Vec<PlanEntry> {
    let slot = spawn.slot.clone();
    vec![
        entry(
            json!({"step": 0}),
            effects(Some(guest_pin(handle)), vec![spawn]),
        ),
        entry(
            json!({"children": {slot: "live"}}),
            effects(None, Vec::new()),
        ),
    ]
}

fn held_child(prompt: &str) -> StreamPlan {
    StreamPlan::current_authored_user(
        prompt,
        vec![StreamResponse::Stream(StreamScript::paused_before(
            prompt,
            vec![StreamChunk::text("child done")],
        ))],
    )
}

/// Boots the fixture job on a request working folder of `cwd`, with room for
/// `capacity` simultaneous provider turns.
async fn boot(
    db: TestDb,
    session_id: &str,
    cwd: &Path,
    tool_ceiling: ToolCeiling,
    accepted_chunks: Vec<StreamChunk>,
    child_plans: Vec<StreamPlan>,
    capacity: usize,
) -> (JobHarness, tempfile::TempDir) {
    let plugin_home = tempfile::tempdir().expect("plugin home");
    let actors = install_job_fixture(&db, plugin_home.path(), job_settings()).await;
    let cwd = cwd.to_string_lossy().into_owned();
    let runtime = boot_managed_accepted_turn_with_backend_capacity(
        &db,
        &actors.fixture,
        AcceptedTurnSpec {
            backend_id: "plugin-job-backend",
            model: MODEL,
            parent_agent_did: &actors.fixture.binding.agent_did,
            configured_agent_dids: &[&actors.fixture.binding.agent_did, &actors.child_did],
            request_id: &format!("{session_id}-parent"),
            session_id,
            prompt: &format!("{session_id} start"),
            accepted_chunks,
            child_plans,
            valid_until: None,
            request_hop: None,
            request_setup: Some(Box::new(move |request| request.input.cwd = Some(cwd))),
        },
        DocumentRuntimeOptions {
            plugin_home: Some(plugin_home.path().to_path_buf()),
            tool_ceiling,
            ..Default::default()
        },
        capacity,
    )
    .await;
    let harness = JobHarness {
        db,
        actors,
        plugin_home: plugin_home.path().to_path_buf(),
        runtime,
        session_id: session_id.into(),
    };
    (harness, plugin_home)
}

/// The child requests the job row caused.
async fn child_requests(h: &JobHarness, row_doc_id: &str) -> usize {
    let response = h.actors.fixture.access.clone()
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{}" }} }}) {{ node_did }} }}"#,
            gents_store::graphql::escape_graphql_string(row_doc_id),
        ))
        .await
        .expect("read the child requests");
    response["data"]["AgentRequest"]
        .as_array()
        .expect("a request list")
        .len()
}

/// The row a refused call left, once it failed.
async fn failed_row(h: &JobHarness, tool_call_id: &str) -> Option<String> {
    let response = h.actors.fixture.access.clone()
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }}, tool_call_id: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(&h.session_id),
            gents_store::graphql::escape_graphql_string(tool_call_id),
        ))
        .await
        .expect("read the call row");
    let row = response["data"]["AgentToolCall"].get(0)?;
    (row["lifecycle_state"] == "failed").then(|| row["_docID"].as_str().unwrap().to_owned())
}

/// The `code` of the call's canonical terminal output.
async fn terminal_code(h: &JobHarness, tool_call_id: &str) -> Option<String> {
    let history =
        crate::support::fixtures::held_fixture_history(&h.db, &h.actors.fixture, &h.session_id)
            .await;
    history.iter().find_map(|message| match message {
        Message::User { content } => content.iter().find_map(|content| match content {
            UserContent::ToolResult(result)
                if result.id == tool_call_id || result.call_id.as_deref() == Some(tool_call_id) =>
            {
                result.content.iter().find_map(|content| match content {
                    ToolResultContent::Text(Text { text }) => {
                        let output: Value = serde_json::from_str(text).ok()?;
                        Some(output["code"].as_str()?.to_owned())
                    }
                    _ => None,
                })
            }
            _ => None,
        }),
        _ => None,
    })
}

/// A call is settled once it is a running job holding one live child, or a
/// failed row refused with `refusal`. A refused step 0 leaves no `job_pin`,
/// so its view is `NotAJob` and the refusal is read from the row's
/// lifecycle state and canonical terminal output (71.1).
enum Settled {
    Running,
    Refused(String),
}

async fn settle(h: &JobHarness, tool_call_id: &str, refusal: &str) -> Settled {
    let access = h.actors.fixture.access.clone();
    let key = || JobKey::Address {
        session_id: h.session_id.clone(),
        tool_call_id: tool_call_id.into(),
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut changes = gents_store::subscription_source::subscribe_collections(
        h.db.node.as_ref(),
        ["AgentToolCall", "AgentRequest", "AgentMessage"],
    )
    .expect("observe the job documents");
    loop {
        let view = load_job_view(&access, None, key()).await;
        match &view {
            Ok(view) if view.state == JobState::Failed => {
                assert_eq!(
                    view.outcome.as_ref().map(|o| o.code.as_str()),
                    Some(refusal)
                );
                return Settled::Refused(view.row_doc_id.clone());
            }
            Ok(view)
                if view.state == JobState::Running
                    && view.children.len() == 1
                    && view.children[0].state == JobChildState::Live =>
            {
                return Settled::Running;
            }
            Ok(_) | Err(JobReadError::NotYetObservable) => {}
            Err(JobReadError::NotAJob) => {
                // The failed row commits before its tool result joins the
                // caller's history; settle once both are observable.
                if let Some(row) = failed_row(h, tool_call_id).await {
                    if let Some(code) = terminal_code(h, tool_call_id).await {
                        assert_eq!(code, refusal, "{tool_call_id}");
                        return Settled::Refused(row);
                    }
                }
            }
            Err(error) => panic!("job {tool_call_id} is unreadable: {error:?}"),
        }
        tokio::time::timeout_at(deadline, changes.recv())
            .await
            .unwrap_or_else(|_| panic!("{tool_call_id} did not settle; last read: {view:?}"))
            .expect("job document subscription closed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_concurrent_adoptions_on_one_request_leave_exactly_eight() {
    let ids: Vec<String> = (1..=9).map(|n| format!("cap-{n}")).collect();
    let prompts: Vec<String> = ids.iter().map(|id| format!("{id} child")).collect();
    let calls = ids
        .iter()
        .zip(&prompts)
        .map(|(id, prompt)| job_call(id, &plan(&format!("{id}-handle"), spawn("a", prompt))))
        .collect();
    let db = test_db("plugin-job-cap").await;
    let cwd = db.data_path().join("job_cwd");
    std::fs::create_dir_all(&cwd).expect("create the job working folder");
    let cwd = cwd.canonicalize().expect("resolve the job working folder");
    let (h, _home) = boot(
        db,
        "plugin-job-cap-session",
        &cwd,
        ToolCeiling::default(),
        calls,
        prompts.iter().map(|prompt| held_child(prompt)).collect(),
        12,
    )
    .await;

    let mut running = 0;
    let mut refused_rows = Vec::new();
    for id in &ids {
        match settle(&h, id, CAPACITY_EXCEEDED).await {
            Settled::Running => running += 1,
            Settled::Refused(row) => refused_rows.push(row),
        }
    }
    assert_eq!(running, 8, "{refused_rows:?}");
    assert_eq!(refused_rows.len(), 1, "{running} running");
    assert_eq!(child_requests(&h, &refused_rows[0]).await, 0);
    h.runtime.shutdown().await;
}
