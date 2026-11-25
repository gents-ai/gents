use std::path::{Path, PathBuf};

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_toolcalls::background::job::job::{JobChildState, JobState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::plugin_job_support::{
    boot_job_harness, effects, guest_pin, job_call, job_child_states, job_row,
    reboot_job_harness_observing_children, result, spawn, wait_job, JobHarness, PlanEntry,
};
use crate::support::trial_store_at;
use gents_inference::test_support::scripted_backend::{
    StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use gents_store::test_support::snapshot_reader::SnapshotReader;
use gents_store::test_support::STALL_CEILING;
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
    child_ids: Vec<String>,
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
    job_child_states(&h.actors.fixture.access, row_doc_id).await
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
    let db = trial_store_at(home.path()).await;
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
    wait_job(&h, TOOL_CALL, |v| {
        v.children.len() == 3 && v.children.iter().all(|c| c.state == JobChildState::Live)
    })
    .await;
    let row_doc_id = job_row(&h, TOOL_CALL).await;
    let mut changes = h
        .actors
        .fixture
        .access
        .subscribe_changes("AgentRequest")
        .await;
    let child_states = tokio::time::timeout(STALL_CEILING, async {
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
        data_path: h.db.path().to_path_buf(),
        plugin_home: h.plugin_home.clone(),
        child_ids: child_states.into_iter().map(|(id, _)| id).collect(),
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
        .arg("e2e_background::plugin_job_restart::plugin_job_survives_a_restart_mid_panel")
        .arg("--nocapture")
        .env(JOB_CRASH_WORKER_READY, &ready_path)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn the job crash worker");
    let ready = tokio::time::timeout(STALL_CEILING, async {
        let (mut connection, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        connection.read_to_end(&mut bytes).await.unwrap();
        serde_json::from_slice::<JobCrashReady>(&bytes).unwrap()
    })
    .await
    .expect("job crash worker readiness");
    child.start_kill().expect("kill the job crash worker");
    child.wait().await.expect("reap the job crash worker");
    assert_eq!(ready.child_ids.len(), 3, "{:?}", ready.child_ids);

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
    let (h, crashed_states) = reboot_job_harness_observing_children(
        &ready.data_path,
        ready.plugin_home.clone(),
        SESSION,
        &ready.row_doc_id,
        restart_plans,
    )
    .await;
    let crashed_ids: Vec<&String> = crashed_states.iter().map(|(id, _)| id).collect();
    let ready_ids: Vec<&String> = ready.child_ids.iter().collect();
    assert_eq!(
        crashed_ids, ready_ids,
        "retained children differ from pre-crash children"
    );
    assert!(
        crashed_states
            .iter()
            .any(|(_, state)| state == "processing"),
        "expected at least one processing child at crash: {crashed_states:?}"
    );

    let view = wait_job(&h, TOOL_CALL, |v| {
        matches!(
            v.state,
            JobState::Completed | JobState::Failed | JobState::Cancelled | JobState::TimedOut
        )
    })
    .await;
    assert_eq!(view.state, JobState::Completed, "{view:?}");

    let row = job_row_fields(&h, &ready.row_doc_id).await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert!(row["cancel_cause"].is_null(), "{row}");
    assert!(!row["plugin_execution_receipt"].is_null(), "{row}");
    let finals = child_states(&h, &ready.row_doc_id).await;
    let final_ids: Vec<&String> = finals.iter().map(|(id, _)| id).collect();
    assert_eq!(final_ids, crashed_ids, "the job gained or lost a child");
    for ((id, before), (_, after)) in crashed_states.iter().zip(&finals) {
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
