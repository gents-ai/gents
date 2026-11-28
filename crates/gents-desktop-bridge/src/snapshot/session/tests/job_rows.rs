use super::*;
use crate::tests::support::{
    fixture_job_progress, seed_served_job_row, seed_served_tool_call, served_desktop, ServedDesktop,
};
use crate::types::RenderedToolCallView;
use gents_protocol::job::{JobOrigin, JobOutcome, JobPauseReason, JobProgress, JobState};

fn progress(state: JobState, pause_reason: Option<JobPauseReason>) -> JobProgress {
    fixture_job_progress("Job", state, pause_reason)
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
    let mut publications = served.core.stores().subscribe();
    while !served
        .core
        .stores()
        .snapshot(node)
        .sessions
        .iter()
        .any(|session| session.session_id == session_id)
    {
        publications
            .changed()
            .await
            .expect("store observer is running");
    }
    let access = served.runtime.user_access();
    let page = gents_observe::client_projection::query::load_session_transcript_page_on(
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
        SessionReadInputs {
            transcript_store: Some(&page.store),
            canonical_dependencies: Some(&page.canonical_dependencies),
            context_store: None,
            prompt_ownership: None,
            context_totals_exact: false,
            include_live_tail: true,
            request_projection: None,
        },
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
async fn job_rows_project_progress_paused_non_job_and_omitted_step_state() {
    let served = served_desktop().await;

    // Failed and cancelled job rows project progress, handle, state, outcome, and next calls.
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

    // Paused job row has state Paused and no outcome.
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
    let paused_snapshot = snapshot_of(&served, "sess-pause", &["req-call-paused"]).await;
    let paused_next = gents_server::jobs::next_calls(JobState::Paused, "sess-pause:call-paused");
    let paused_wire = rendered(&paused_snapshot, "call-paused");
    assert_eq!(paused_wire.job_state, Some(JobState::Paused));
    assert_eq!(paused_wire.job_origin, Some(JobOrigin::Installed));
    assert_eq!(paused_wire.job_outcome, None);
    assert_eq!(paused_wire.job_next_calls, paused_next);
    let paused_flat = flat(&paused_snapshot, "call-paused");
    assert_eq!(paused_flat.job_state, Some(JobState::Paused));
    assert_eq!(paused_flat.job_outcome, None);
    assert_eq!(paused_flat.job_next_calls, paused_next);

    // Plain tool call beside a job row carries no job fields, and jobStepState is not projected.
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
    let mixed = snapshot_of(&served, "sess_parent", &["req-call-job"]).await;
    let plain_wire = rendered(&mixed, "call-plain");
    assert_eq!(plain_wire.job_progress, None);
    assert_eq!(plain_wire.job_handle, None);
    assert_eq!(plain_wire.job_state, None);
    assert_eq!(plain_wire.job_outcome, None);
    assert!(plain_wire.job_next_calls.is_empty());
    let plain_flat = flat(&mixed, "call-plain");
    assert_eq!(plain_flat.job_progress, None);
    assert_eq!(plain_flat.job_state, None);
    assert!(plain_flat.job_next_calls.is_empty());
    let plain_value = serde_json::to_value(plain_wire).expect("serialize rendered tool");
    for key in [
        "jobProgress",
        "jobHandle",
        "jobState",
        "jobOrigin",
        "jobOutcome",
        "jobNextCalls",
    ] {
        assert!(
            plain_value.get(key).is_none(),
            "{key} on a non-job row: {plain_value}"
        );
    }
    let job_value = serde_json::to_value(rendered(&mixed, "call-job")).expect("serialize");
    assert!(job_value.get("jobStepState").is_none(), "{job_value}");
    assert!(job_value.get("jobState").is_some(), "{job_value}");

    served.shutdown().await;
}
