use gents_protocol::job::{JobBudget, JobOutcome, JobProgress, JobState};

use super::support::{seed_served_job_row, served_desktop};
use crate::error::BridgeErrorCode;
use crate::job_view::{job_view, jobs_list};
use gents_protocol::operations::job::JobListInput;

fn progress(state: JobState) -> JobProgress {
    JobProgress {
        title: "Review".into(),
        state,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 1,
        budget: JobBudget {
            total: 4,
            spent: 1,
            remaining: 3,
        },
        last_event: "seeded".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    }
}

#[tokio::test]
async fn job_reads_list_and_view_served_rows_through_operator_access() {
    let served = served_desktop().await;
    let node = served.node_did().to_owned();
    let failed_row = seed_served_job_row(
        &served,
        "sess-reads",
        "call-failed",
        "failed",
        &progress(JobState::Failed),
        Some(("engine_boom", "boom")),
    )
    .await;
    seed_served_job_row(
        &served,
        "sess-reads",
        "call-running",
        "running",
        &progress(JobState::Running),
        None,
    )
    .await;

    let all = jobs_list(
        &served.core,
        JobListInput {
            node_did: node.clone(),
            ..Default::default()
        },
    )
    .await
    .expect("every job row");
    let mut addresses: Vec<_> = all.iter().map(|view| view.address.as_str()).collect();
    addresses.sort_unstable();
    assert_eq!(
        addresses,
        ["sess-reads:call-failed", "sess-reads:call-running"]
    );

    let running = jobs_list(
        &served.core,
        JobListInput {
            node_did: node.clone(),
            states: Some(vec![JobState::Running]),
            plugin: Some("fixture/workflow".into()),
            session_id: Some("sess-reads".into()),
        },
    )
    .await
    .expect("running rows");
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].tool_call_id, "call-running");
    let other_plugin = jobs_list(
        &served.core,
        JobListInput {
            node_did: node.clone(),
            plugin: Some("other/workflow".into()),
            ..Default::default()
        },
    )
    .await
    .expect("no rows");
    assert!(other_plugin.is_empty());

    let by_address = job_view(
        &served.core,
        &node,
        Some("sess-reads:call-failed".into()),
        None,
    )
    .await
    .expect("view by address");
    assert_eq!(by_address.state, JobState::Failed);
    assert_eq!(by_address.row_doc_id, failed_row);
    assert_eq!(
        by_address.outcome,
        Some(JobOutcome {
            code: "engine_boom".into(),
            message: "boom".into(),
        })
    );
    let by_row = job_view(&served.core, &node, None, Some(failed_row))
        .await
        .expect("view by row");
    assert_eq!(by_row, by_address);

    let missing = job_view(
        &served.core,
        &node,
        Some("sess-reads:call-missing".into()),
        None,
    )
    .await
    .expect_err("no such row");
    assert_eq!(missing.code, BridgeErrorCode::NotFound);
    served.shutdown().await;
}
