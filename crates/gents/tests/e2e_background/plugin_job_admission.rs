//! A plugin job's child cwd and the per-request background cap refuse closed.

use std::path::Path;
use std::time::Duration;

use gents::llm::message::{Message, Text, ToolResultContent, UserContent};
use gents::plugin_job::job::{
    JobChildState, JobReadError, JobSpawn, JobState, JobStepOutput, JobView,
};
use gents::plugin_job::view::{load_job_view, JobKey};
use gents::{ConfigAccess, DocumentRuntimeOptions, ToolCeiling};
use serde_json::{json, Value};

use crate::plugin_job_support::{
    effects, guest_pin, install_job_fixture, job_call, job_settings, result, spawn, wait_job,
    JobHarness, PlanEntry, CHILD, MODEL, PARENT,
};
use crate::support::accepted_turn::{boot_accepted_turn_with_backend_capacity, AcceptedTurnSpec};
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use crate::support::{test_db, TestDb};

const POLICY_DENIED: &str = "plugin_job_policy_denied";
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
    install_job_fixture(&db, plugin_home.path(), job_settings()).await;
    let cwd = cwd.to_string_lossy().into_owned();
    let runtime = boot_accepted_turn_with_backend_capacity(
        &db,
        AcceptedTurnSpec {
            backend_id: "plugin-job-backend",
            model: MODEL,
            parent_agent_id: PARENT,
            configured_agent_ids: &[PARENT, CHILD],
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
        plugin_home: plugin_home.path().to_path_buf(),
        runtime,
        session_id: session_id.into(),
    };
    (harness, plugin_home)
}

/// The child requests the job row caused.
async fn child_requests(h: &JobHarness, row_doc_id: &str) -> usize {
    let response = ConfigAccess::local(h.db.node.clone())
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{}" }} }}) {{ node_did }} }}"#,
            gents::graphql::escape_graphql_string(row_doc_id),
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
    let response = ConfigAccess::local(h.db.node.clone())
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }}, tool_call_id: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state }} }}"#,
            gents::graphql::escape_graphql_string(&h.session_id),
            gents::graphql::escape_graphql_string(tool_call_id),
        ))
        .await
        .expect("read the call row");
    let row = response["data"]["AgentToolCall"].get(0)?;
    (row["lifecycle_state"] == "failed").then(|| row["_docID"].as_str().unwrap().to_owned())
}

/// The `code` of the call's canonical terminal output.
async fn terminal_code(h: &JobHarness, tool_call_id: &str) -> Option<String> {
    let did = h.db.node_identity.did();
    let history = gents::load_history(h.db.node.as_ref(), &h.session_id, did, Some(did))
        .await
        .expect("load the canonical history");
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
    Running(Box<JobView>),
    Refused(String),
}

async fn settle(h: &JobHarness, tool_call_id: &str, refusal: &str) -> Settled {
    let access = ConfigAccess::local(h.db.node.clone());
    let key = || JobKey::Address {
        session_id: h.session_id.clone(),
        tool_call_id: tool_call_id.into(),
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
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
                return Settled::Running(Box::new(view.clone()));
            }
            Ok(_) | Err(JobReadError::NotYetObservable) => {}
            Err(JobReadError::NotAJob) => {
                if let Some(row) = failed_row(h, tool_call_id).await {
                    let code = terminal_code(h, tool_call_id).await;
                    assert_eq!(code.as_deref(), Some(refusal), "{tool_call_id}");
                    return Settled::Refused(row);
                }
            }
            Err(error) => panic!("job {tool_call_id} is unreadable: {error:?}"),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{tool_call_id} did not settle; last read: {view:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn refused(h: &JobHarness, tool_call_id: &str) -> String {
    match settle(h, tool_call_id, POLICY_DENIED).await {
        Settled::Refused(row) => row,
        Settled::Running(view) => panic!("{tool_call_id} was not refused: {view:?}"),
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_cwd_with_dotdot_symlink_or_outside_caller_cwd_fails_closed_with_no_child() {
    let db = test_db("plugin-job-spawn-cwd").await;
    let base = db.data_path().canonicalize().expect("resolve the store");
    let cwd = base.join("job_cwd");
    let outside = base.join("outside");
    std::fs::create_dir_all(&cwd).expect("create the job working folder");
    std::fs::create_dir_all(&outside).expect("create the outside folder");
    std::os::unix::fs::symlink(&outside, cwd.join("link")).expect("link out of the folder");

    let cases = [
        ("cwd-dotdot", "../outside".to_owned()),
        ("cwd-link", "link".to_owned()),
        ("cwd-outside", outside.to_string_lossy().into_owned()),
    ];
    let calls = cases
        .iter()
        .map(|(id, dir)| {
            let spawn = JobSpawn {
                cwd: Some(dir.clone()),
                ..spawn("a", &format!("{id} child"))
            };
            job_call(id, &plan(&format!("{id}-handle"), spawn))
        })
        .collect();
    let (h, _home) = boot(
        db,
        "plugin-job-spawn-cwd-session",
        &cwd,
        ToolCeiling::default(),
        calls,
        Vec::new(),
        4,
    )
    .await;

    for (id, dir) in &cases {
        let row = refused(&h, id).await;
        assert_eq!(child_requests(&h, &row).await, 0, "{id}: {dir}");
    }
    h.runtime.shutdown().await;
}

const OPERATOR_ROOT_WORKER: &str = "GENTS_PLUGIN_JOB_OPERATOR_ROOT_WORKER";

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_cwd_outside_the_operator_root_is_refused_by_the_child_admission() {
    // Request admission reads the process-wide operator root, which every
    // runtime boot in this binary replaces, so the scenario runs alone.
    if std::env::var_os(OPERATOR_ROOT_WORKER).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("plugin_job_admission::child_cwd_outside_the_operator_root_is_refused_by_the_child_admission")
            .arg("--nocapture")
            .env(OPERATOR_ROOT_WORKER, "1")
            .status()
            .expect("run the operator root scenario");
        assert!(status.success(), "operator root scenario: {status}");
        return;
    }
    let db = test_db("plugin-job-operator-root").await;
    let base = db.data_path().canonicalize().expect("resolve the store");
    let root = base.join("root");
    let cwd = root.join("job_cwd");
    let outside = base.join("outside");
    std::fs::create_dir_all(&cwd).expect("create the job working folder");
    std::fs::create_dir_all(&outside).expect("create the outside folder");

    // A spawn without a cwd inherits the pinned caller cwd, which only the
    // child's own admission resolves again.
    let plan = [
        entry(
            json!({"step": 0}),
            effects(
                Some(guest_pin("root-1-handle")),
                vec![spawn("a", "root child a")],
            ),
        ),
        entry(
            json!({"children": {"b": "failed"}}),
            result(json!("b refused")),
        ),
        entry(
            json!({"children": {"b": "live"}}),
            effects(None, Vec::new()),
        ),
        entry(
            json!({"children": {"a": "completed"}}),
            effects(None, vec![spawn("b", "root child b")]),
        ),
        entry(
            json!({"children": {"a": "live"}}),
            effects(None, Vec::new()),
        ),
    ];
    let (h, _home) = boot(
        db,
        "plugin-job-operator-root-session",
        &cwd,
        ToolCeiling::readonly_at(&root),
        vec![job_call("root-1", &plan)],
        vec![held_child("root child a")],
        4,
    )
    .await;
    // Child a's setup resolves its cwd, so the folder moves only once its
    // provider turn is held.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while h.runtime.backend.observed_requests("root child a") == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "child a never reached its provider turn"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The pinned cwd now resolves outside the operator root.
    std::fs::rename(&cwd, root.join("job_cwd_was")).expect("move the job working folder");
    std::os::unix::fs::symlink(&outside, &cwd).expect("link the pinned cwd outside the root");
    h.runtime.backend.release("root child a");

    let view = wait_job(
        &h,
        "root-1",
        |view| {
            view.children
                .iter()
                .any(|child| child.slot == "b" && child.state == JobChildState::Failed)
        },
        Duration::from_secs(60),
    )
    .await;
    let response = ConfigAccess::local(h.db.node.clone())
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{}" }} }}) {{ lifecycle_state failure_reason }} }}"#,
            gents::graphql::escape_graphql_string(&view.row_doc_id),
        ))
        .await
        .expect("read the child requests");
    let failed: Vec<&Value> = response["data"]["AgentRequest"]
        .as_array()
        .expect("a request list")
        .iter()
        .filter(|request| request["lifecycle_state"] == "failed")
        .collect();
    assert_eq!(failed.len(), 1, "{response}");
    let reason = failed[0]["failure_reason"].as_str().unwrap();
    assert!(
        reason.contains("request admission denied")
            && reason.contains("escapes operator tool root"),
        "{reason}"
    );
    h.runtime.shutdown().await;
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
            Settled::Running(_) => running += 1,
            Settled::Refused(row) => refused_rows.push(row),
        }
    }
    assert_eq!(running, 8, "{refused_rows:?}");
    assert_eq!(refused_rows.len(), 1, "{running} running");
    assert_eq!(child_requests(&h, &refused_rows[0]).await, 0);
    h.runtime.shutdown().await;
}
