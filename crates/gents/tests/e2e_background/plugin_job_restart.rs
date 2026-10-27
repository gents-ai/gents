//! A plugin job survives a process kill while its children are live.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gents::plugin_job::job::{JobChildState, JobState};
use gents::ConfigAccess;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::plugin_job_support::{
    boot_job_harness, effects, guest_pin, job_call, job_row, reboot_job_harness, result, spawn,
    wait_job, JobHarness, PlanEntry,
};
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use crate::support::test_db_in;

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

fn entry(when: Value, output: gents::plugin_job::job::JobStepOutput) -> PlanEntry {
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

/// The children's (`_docID`, `lifecycle_state`), sorted by `_docID`.
async fn child_states(h: &JobHarness, row_doc_id: &str) -> Vec<(String, String)> {
    let response = ConfigAccess::local(h.db.node.clone())
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state }} }}"#,
            gents::graphql::escape_graphql_string(row_doc_id),
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
    let response = ConfigAccess::local(h.db.node.clone())
        .execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state cancel_cause plugin_execution_receipt }} }}"#,
            gents::graphql::escape_graphql_string(row_doc_id),
        ))
        .await
        .expect("read the job row");
    response["data"]["AgentToolCall"][0].clone()
}

async fn run_job_crash_worker(ready_path: &Path) {
    let handshake = ready_path.parent().expect("handshake parent");
    let db = test_db_in(
        tempfile::Builder::new()
            .prefix("worker-data-")
            .tempdir_in(handshake)
            .unwrap(),
    )
    .await;
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
    let h = boot_job_harness(
        db,
        handshake.join("plugin-home"),
        SESSION,
        vec![job_call(TOOL_CALL, &restart_job_plan())],
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
    // A live child may still be pending. Snapshot only once the backend's one
    // slot holds a paused child: no pending child can be claimed before the kill.
    let child_states = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let states = child_states(&h, &row_doc_id).await;
            if states.iter().any(|(_, state)| state == "processing") {
                break states;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
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
    let pending = ready_path.with_extension("json.pending");
    std::fs::write(&pending, serde_json::to_vec(&ready).unwrap()).unwrap();
    std::fs::rename(pending, ready_path).unwrap();
    std::future::pending::<()>().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_job_survives_a_restart_mid_panel() {
    if let Some(ready_path) = std::env::var_os(JOB_CRASH_WORKER_READY) {
        run_job_crash_worker(Path::new(&ready_path)).await;
        unreachable!();
    }
    let handshake = tempfile::tempdir().unwrap();
    std::fs::create_dir(handshake.path().join("plugin-home")).unwrap();
    let ready_path = handshake.path().join("ready.json");
    struct ChildGuard(Option<std::process::Child>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("plugin_job_restart::plugin_job_survives_a_restart_mid_panel")
        .arg("--nocapture")
        .env(JOB_CRASH_WORKER_READY, &ready_path)
        .spawn()
        .expect("spawn the job crash worker");
    let mut child = ChildGuard(Some(child));
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
                panic!("job crash worker exited before readiness: {status}");
            }
            if let Ok(bytes) = std::fs::read(&ready_path) {
                break serde_json::from_slice::<JobCrashReady>(&bytes).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("job crash worker readiness");
    let mut killed = child.0.take().unwrap();
    killed.kill().expect("kill the job crash worker");
    killed.wait().expect("reap the job crash worker");
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
    tokio::time::sleep(Duration::from_secs(6)).await;
    let later = job_row_fields(&h, &ready.row_doc_id).await;
    assert_eq!(later["lifecycle_state"], row["lifecycle_state"], "{later}");
    assert_eq!(later["cancel_cause"], row["cancel_cause"], "{later}");
    assert_eq!(
        later["plugin_execution_receipt"], row["plugin_execution_receipt"],
        "{later}"
    );
    wait_job(
        &h,
        TOOL_CALL,
        |v| v.state == JobState::Completed,
        Duration::from_secs(1),
    )
    .await;

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
