use std::path::{Path, PathBuf};
use std::time::Duration;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_toolcalls::background::job::job::{JobChildState, JobState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::plugin_job_support::{
    boot_job_harness, effects, guest_pin, job_call, job_row, reboot_job_harness, result, spawn,
    wait_job, JobHarness, PlanEntry,
};
use crate::support::snapshots::SnapshotReader;
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use crate::support::test_db_in;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const JOB_CRASH_WORKER_READY: &str = "GENTS_PLUGIN_JOB_CRASH_WORKER_READY";
const SESSION: &str = "plugin-job-restart-session";
const TOOL_CALL: &str = "job-r";
const SLOTS: [&str; 3] = ["a", "b", "c"];

fn child_prompt(slot: &str) -> String {
    format!("restart child {slot}")
}

#[derive(Serialize, Deserialize)]
struct JobCrashReady {
    data_path: PathBuf,
    plugin_home: PathBuf,
    row_doc_id: String,
    child_states: Vec<(String, String)>,
}

fn entry(when: Value, output: gents_toolcalls::background::job::job::JobStepOutput) -> PlanEntry {
    PlanEntry {
        when,
        output,
        control: None,
    }
}

fn restart_job_plan() -> Vec<PlanEntry> {
    let mut plan = vec![entry(
        json!({"step": 0}),
        effects(
            Some(guest_pin("job-r")),
            SLOTS.map(|s| spawn(s, &child_prompt(s))).to_vec(),
        ),
    )];
    for slot in SLOTS {
        plan.push(entry(
            json!({"children": {slot: "live"}}),
            effects(None, Vec::new()),
        ));
    }
    plan.push(entry(json!({}), result(json!("panel done"))));
    plan
}

async fn child_states(h: &JobHarness, row_doc_id: &str) -> Vec<(String, String)> {
    let response = h.actors.fixture.access.clone()
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(row_doc_id),
        ))
        .await
        .expect("read the job children");
    let mut states: Vec<(String, String)> = response["data"]["AgentRequest"]
        .as_array()
        .expect("children array")
        .iter()
        .map(|row| {
            (
                row["_docID"].as_str().expect("child doc id").to_string(),
                row["lifecycle_state"]
                    .as_str()
                    .expect("child state")
                    .to_string(),
            )
        })
        .collect();
    states.sort();
    states
}

async fn job_row_fields(h: &JobHarness, row_doc_id: &str) -> Value {
    let response = h.actors.fixture.access.clone()
        .execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state cancel_cause plugin_execution_receipt }} }}"#,
            gents_store::graphql::escape_graphql_string(row_doc_id),
        ))
        .await
        .expect("read the job row");
    response["data"]["AgentToolCall"][0].clone()
}

async fn run_job_crash_worker(ready_path: &Path) {
    let handshake = ready_path.parent().expect("handshake parent");
    let parent = handshake.to_owned();
    let home = tokio::task::spawn_blocking(move || {
        tempfile::Builder::new()
            .prefix("worker-data-")
            .tempdir_in(parent)
            .unwrap()
    })
    .await
    .unwrap();
    let db = test_db_in(home).await;
    let child_plans = SLOTS
        .iter()
        .map(|slot| {
            let p = child_prompt(slot);
            StreamPlan::current_authored_user(
                p.clone(),
                vec![StreamResponse::Stream(StreamScript::paused_before(
                    p,
                    vec![StreamChunk::text("child done")],
                ))],
            )
        })
        .collect();
    let plan = tokio::task::spawn_blocking(restart_job_plan).await.unwrap();
    let h = boot_job_harness(
        db,
        handshake.join("plugin-home"),
        SESSION,
        vec![job_call(TOOL_CALL, &plan)],
        child_plans,
    )
    .await;
    wait_job(
        &h,
        TOOL_CALL,
        |v| v.children.len() == 3 && v.children.iter().all(|c| c.state == JobChildState::Live),
        Duration::from_secs(30),
    )
    .await;
    let row_doc_id = job_row(&h, TOOL_CALL).await;
    let mut changes = h
        .actors
        .fixture
        .access
        .subscribe_changes("AgentRequest")
        .await;
    let child_states = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let states = child_states(&h, &row_doc_id).await;
            if states.iter().any(|(_, state)| state == "processing") {
                break states;
            }
            changes.recv().await;
        }
    })
    .await
    .expect("a job child is processing");
    let ready = JobCrashReady {
        data_path: h.db.data_path().to_path_buf(),
        plugin_home: h.plugin_home.clone(),
        child_states,
        row_doc_id,
    };
    let mut connection = tokio::net::UnixStream::connect(ready_path).await.unwrap();
    connection
        .write_all(&serde_json::to_vec(&ready).unwrap())
        .await
        .unwrap();
    connection.shutdown().await.unwrap();
    std::future::pending::<()>().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_job_survives_a_restart_mid_panel() {
    if let Some(ready_path) = std::env::var_os(JOB_CRASH_WORKER_READY) {
        run_job_crash_worker(Path::new(&ready_path)).await;
        unreachable!();
    }
    let handshake = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .unwrap()
        .unwrap();
    tokio::fs::create_dir(handshake.path().join("plugin-home"))
        .await
        .unwrap();
    let ready_path = handshake.path().join("ready.sock");
    let listener = tokio::net::UnixListener::bind(&ready_path).unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("plugin_job_restart::plugin_job_survives_a_restart_mid_panel")
        .arg("--nocapture")
        .env(JOB_CRASH_WORKER_READY, &ready_path)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn the job crash worker");
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        let (mut connection, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        connection.read_to_end(&mut bytes).await.unwrap();
        serde_json::from_slice::<JobCrashReady>(&bytes).unwrap()
    })
    .await
    .expect("job crash worker readiness");
    child.start_kill().expect("kill the job crash worker");
    child.wait().await.expect("reap the job crash worker");
    assert_eq!(ready.child_states.len(), 3, "{:?}", ready.child_states);

    let restart_plans = SLOTS
        .iter()
        .map(|slot| {
            let p = child_prompt(slot);
            StreamPlan::current_authored_user(
                p.clone(),
                vec![StreamResponse::completes(p, ["child done after restart"])],
            )
        })
        .collect();
    let h = reboot_job_harness(
        &ready.data_path,
        ready.plugin_home.clone(),
        SESSION,
        restart_plans,
    )
    .await;

    let row = job_row_fields(&h, &ready.row_doc_id).await;
    assert_eq!(row["lifecycle_state"], "running", "{row}");
    assert!(row["cancel_cause"].is_null(), "{row}");

    let view = wait_job(
        &h,
        TOOL_CALL,
        |v| {
            matches!(
                v.state,
                JobState::Completed | JobState::Failed | JobState::Cancelled | JobState::TimedOut
            )
        },
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(view.state, JobState::Completed, "{view:?}");

    let row = job_row_fields(&h, &ready.row_doc_id).await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert!(row["cancel_cause"].is_null(), "{row}");
    assert!(!row["plugin_execution_receipt"].is_null(), "{row}");
    let crash_ids: Vec<&String> = ready.child_states.iter().map(|(id, _)| id).collect();
    let finals = child_states(&h, &ready.row_doc_id).await;
    let final_ids: Vec<&String> = finals.iter().map(|(id, _)| id).collect();
    assert_eq!(final_ids, crash_ids, "the job gained or lost a child");
    let slots: Vec<(&str, u32)> = view
        .children
        .iter()
        .map(|c| (c.slot.as_str(), c.attempt))
        .collect();
    assert_eq!(slots, [("a", 0), ("b", 0), ("c", 0)], "{view:?}");
    for ((id, before), (_, after)) in ready.child_states.iter().zip(&finals) {
        assert!(
            RequestLifecycleState::is_terminal_str(Some(after)),
            "child {id} ended {after}"
        );
        if before == "pending" {
            assert_eq!(after, "completed", "pending child {id}");
        }
    }
    h.runtime.shutdown().await;
}
