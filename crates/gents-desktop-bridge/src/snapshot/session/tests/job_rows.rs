use super::*;
use crate::tests::support::{
    seed_served_job_row, seed_served_tool_call, served_desktop, ServedDesktop,
};
use crate::types::RenderedToolCallView;
use gents_protocol::job::{
    JobBudget, JobOrigin, JobOutcome, JobPauseReason, JobProgress, JobState,
};

fn progress(state: JobState, pause_reason: Option<JobPauseReason>) -> JobProgress {
    JobProgress {
        title: "Job".into(),
        state,
        pause_reason,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 2,
        budget: JobBudget {
            total: 4,
            spent: 2,
            remaining: 2,
        },
        last_event: "seeded".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    }
}

async fn snapshot_of(
    served: &ServedDesktop,
    session_id: &str,
    request_ids: &[&str],
) -> DesktopSessionSnapshot {
    let node = served.node_did();
    let user = served.core.node_identity().did().to_owned();
    for request_id in request_ids {
        served
            .core
            .refresh_local_request(node, session_id, Some(request_id), None, &[])
            .await
            .expect("refresh request");
    }
    let access = served.runtime.user_access();
    let page = gents_desktop_core::client::load_session_transcript_page_on(
        &access,
        session_id,
        Some(node),
        Some(&user),
        None,
        None,
    )
    .await
    .expect("transcript page");
    build_session_snapshot_for_node_with_transcript(
        served.core.as_ref(),
        &access,
        Some(node),
        session_id,
        None,
        Some(&page.store),
        Some(&page.canonical_dependencies),
        None,
        None,
        false,
        true,
        None,
    )
    .await
    .expect("session snapshot read")
    .expect("session snapshot")
}

fn rendered<'a>(snapshot: &'a DesktopSessionSnapshot, call: &str) -> &'a RenderedToolCallView {
    snapshot
        .timeline_items
        .iter()
        .find_map(|item| match item {
            RenderedTimelineItem::ToolGroup { tools, .. } => tools
                .iter()
                .find(|tool| tool.tool_call_id.as_deref() == Some(call)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("rendered tool row {call}"))
}

fn flat<'a>(snapshot: &'a DesktopSessionSnapshot, call: &str) -> &'a ToolCallView {
    snapshot
        .tool_calls
        .iter()
        .find(|tool| tool.tool_call_id.as_deref() == Some(call))
        .unwrap_or_else(|| panic!("tool row {call}"))
}

#[tokio::test]
async fn job_row_projects_progress_handle_state_and_outcome() {
    let served = served_desktop().await;
    let failed = progress(JobState::Failed, None);
    seed_served_job_row(
        &served,
        "sess-job",
        "call-failed",
        "failed",
        &failed,
        Some(("engine_boom", "boom")),
    )
    .await;
    seed_served_job_row(
        &served,
        "sess-job",
        "call-cancelled",
        "cancelled",
        &progress(JobState::Running, None),
        None,
    )
    .await;

    let snapshot = snapshot_of(
        &served,
        "sess-job",
        &["req-call-failed", "req-call-cancelled"],
    )
    .await;

    let outcome = JobOutcome {
        code: "engine_boom".into(),
        message: "boom".into(),
    };
    let next = gents_server::jobs::next_calls(JobState::Failed, "sess-job:call-failed");
    let wire = rendered(&snapshot, "call-failed");
    let flat_row = flat(&snapshot, "call-failed");
    assert_eq!(wire.job_progress.as_ref(), Some(&failed));
    assert_eq!(flat_row.job_progress.as_ref(), Some(&failed));
    for (handle, state, job_outcome, next_calls) in [
        (
            &wire.job_handle,
            &wire.job_state,
            &wire.job_outcome,
            &wire.job_next_calls,
        ),
        (
            &flat_row.job_handle,
            &flat_row.job_state,
            &flat_row.job_outcome,
            &flat_row.job_next_calls,
        ),
    ] {
        assert_eq!(handle.as_deref(), Some("call-failed"));
        assert_eq!(*state, Some(JobState::Failed));
        assert_eq!(job_outcome.as_ref(), Some(&outcome));
        assert_eq!(next_calls, &next);
    }

    let cancelled = rendered(&snapshot, "call-cancelled");
    assert_eq!(cancelled.job_state, Some(JobState::Cancelled));
    assert_eq!(cancelled.job_outcome, None);
    assert_eq!(
        flat(&snapshot, "call-cancelled").job_state,
        Some(JobState::Cancelled)
    );
    assert_eq!(flat(&snapshot, "call-cancelled").job_outcome, None);
    served.shutdown().await;
}

#[tokio::test]
async fn paused_job_row_has_state_paused_and_no_outcome() {
    let served = served_desktop().await;
    let paused = progress(JobState::Paused, Some(JobPauseReason::BudgetExceeded));
    seed_served_job_row(
        &served,
        "sess-pause",
        "call-paused",
        "completed",
        &paused,
        None,
    )
    .await;

    let snapshot = snapshot_of(&served, "sess-pause", &["req-call-paused"]).await;

    let next = gents_server::jobs::next_calls(JobState::Paused, "sess-pause:call-paused");
    let wire = rendered(&snapshot, "call-paused");
    assert_eq!(wire.job_state, Some(JobState::Paused));
    assert_eq!(wire.job_origin, Some(JobOrigin::Installed));
    assert_eq!(wire.job_outcome, None);
    assert_eq!(wire.job_next_calls, next);
    let flat_row = flat(&snapshot, "call-paused");
    assert_eq!(flat_row.job_state, Some(JobState::Paused));
    assert_eq!(flat_row.job_outcome, None);
    assert_eq!(flat_row.job_next_calls, next);
    served.shutdown().await;
}

#[tokio::test]
async fn non_job_row_has_no_job_fields() {
    let served = served_desktop().await;
    seed_served_job_row(
        &served,
        "sess_parent",
        "call-job",
        "completed",
        &progress(JobState::Completed, None),
        None,
    )
    .await;
    seed_served_tool_call(
        &served,
        "sess_parent",
        "req-call-job",
        "call-plain",
        "read_file",
    )
    .await;

    let snapshot = snapshot_of(&served, "sess_parent", &["req-call-job"]).await;

    let wire = rendered(&snapshot, "call-plain");
    assert_eq!(wire.job_progress, None);
    assert_eq!(wire.job_handle, None);
    assert_eq!(wire.job_state, None);
    assert_eq!(wire.job_outcome, None);
    assert!(wire.job_next_calls.is_empty());
    let flat_row = flat(&snapshot, "call-plain");
    assert_eq!(flat_row.job_progress, None);
    assert_eq!(flat_row.job_state, None);
    assert!(flat_row.job_next_calls.is_empty());
    let value = serde_json::to_value(wire).expect("serialize rendered tool");
    for key in [
        "jobProgress",
        "jobHandle",
        "jobState",
        "jobOrigin",
        "jobOutcome",
        "jobNextCalls",
    ] {
        assert!(value.get(key).is_none(), "{key} on a non-job row: {value}");
    }
    served.shutdown().await;
}

#[tokio::test]
async fn job_step_state_is_not_projected() {
    let served = served_desktop().await;
    seed_served_job_row(
        &served,
        "sess-step",
        "call-step",
        "completed",
        &progress(JobState::Completed, None),
        None,
    )
    .await;

    let snapshot = snapshot_of(&served, "sess-step", &["req-call-step"]).await;

    let value = serde_json::to_value(rendered(&snapshot, "call-step")).expect("serialize");
    assert!(value.get("jobStepState").is_none(), "{value}");
    assert!(value.get("jobState").is_some(), "{value}");
    served.shutdown().await;
}
