use chrono::Utc;
use defra_agent::tool_call_lifecycle::{CancelCause, ToolCallLifecycle};

use crate::lean_vocab_test::{
    lean_composed_invariant_witness, lean_composed_invariant_witnesses,
    LeanComposedInvariantWitness,
};
use crate::support::snapshots::fetch_tool_call_snapshots_for_session;
use crate::support::{create_request, test_db, AGENT_DID};

const C1: &str = "ComposedState.deadline_exceeded_request_timesOut_running_tools_from_initial";
const C1_PRIME: &str = "ComposedState.deadline_exceeded_request_cancels_pending_tools_from_initial";

pub(super) async fn generated_composed_invariant_witnesses_drive_tool_lifecycle_conformance() {
    let witnesses = lean_composed_invariant_witnesses();
    assert_eq!(witnesses.len(), 2);

    drive_running_deadline_witness(lean_composed_invariant_witness(C1)).await;
    drive_pending_deadline_witness(lean_composed_invariant_witness(C1_PRIME)).await;
}

async fn drive_running_deadline_witness(witness: &LeanComposedInvariantWitness) {
    assert_common_reachable_deadline_shape(witness);
    assert_eq!(
        witness.scenario,
        "running_tool_deadline_exceeded_times_out_on_recovery"
    );
    assert_eq!(witness.rust_path, "ToolCallLifecycle::recover_all");
    assert_eq!(witness.trace_step_count, witness.transition_path.len());
    assert_eq!(witness.tool_pre_state, "running");
    assert_eq!(witness.tool_post_state, "timedOut");
    assert!(witness.pre_tool_persisted);

    let db = test_db("composed-c1-running-deadline").await;
    let request_id = format!("composed-c1-request-{}", witness.request_id);
    let session_id = "composed-c1-session";
    let tool_call_id = format!("composed-c1-tool-{}", witness.tool_call_id);
    create_request(
        &db.node,
        &request_id,
        session_id,
        &witness.pre_request_state,
        "2024-01-01T00:00:00Z",
    )
    .await;

    let mut lifecycle = ToolCallLifecycle::new(
        db.node.clone(),
        request_id,
        session_id.to_string(),
        AGENT_DID.to_string(),
        tool_call_id,
        0,
        "slow_tool".to_string(),
        "{}".to_string(),
        Utc::now() - chrono::Duration::seconds(5),
    );
    lifecycle.start_running().await.unwrap();

    let report = ToolCallLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, session_id).await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some(witness.tool_post_state.as_str())
    );
    assert_eq!(
        snapshots[0].cancel_cause.as_deref(),
        witness.cancel_cause.as_deref()
    );
    assert_eq!(snapshots[0].tool_failure_class.as_deref(), Some("external"));
}

async fn drive_pending_deadline_witness(witness: &LeanComposedInvariantWitness) {
    assert_common_reachable_deadline_shape(witness);
    assert_eq!(
        witness.scenario,
        "pending_tool_deadline_exceeded_cancels_before_dispatch"
    );
    assert_eq!(
        witness.rust_path,
        "ToolCallLifecycle::cancel_before_dispatch"
    );
    assert_eq!(witness.trace_step_count, witness.transition_path.len());
    assert_eq!(witness.tool_pre_state, "pending");
    assert_eq!(witness.tool_post_state, "cancelled");
    assert!(!witness.pre_tool_persisted);

    let db = test_db("composed-c1-prime-pending-deadline").await;
    let request_id = format!("composed-c1-prime-request-{}", witness.request_id);
    let session_id = "composed-c1-prime-session";
    let tool_call_id = format!("composed-c1-prime-tool-{}", witness.tool_call_id);
    create_request(
        &db.node,
        &request_id,
        session_id,
        &witness.pre_request_state,
        "2024-01-01T00:00:00Z",
    )
    .await;

    let mut lifecycle = ToolCallLifecycle::new(
        db.node.clone(),
        request_id,
        session_id.to_string(),
        AGENT_DID.to_string(),
        tool_call_id,
        0,
        "slow_tool".to_string(),
        "{}".to_string(),
        Utc::now() - chrono::Duration::seconds(5),
    );
    lifecycle
        .cancel_before_dispatch(CancelCause::Deadline)
        .await
        .unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, session_id).await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some(witness.tool_post_state.as_str())
    );
    assert_eq!(
        snapshots[0].cancel_cause.as_deref(),
        witness.cancel_cause.as_deref()
    );
    assert_eq!(snapshots[0].tool_failure_class.as_deref(), None);
}

fn assert_common_reachable_deadline_shape(witness: &LeanComposedInvariantWitness) {
    assert_eq!(witness.witness_kind, "reachable_domain");
    assert_eq!(witness.pre_request_state, "processing");
    assert_eq!(witness.pre_request_admission, "executing");
    assert_eq!(witness.request_id, witness.tool_request_id);
    assert_eq!(witness.request_deadline, witness.tool_deadline);
    assert_eq!(witness.request_current_time, witness.tool_current_time);
    assert!(
        witness.request_current_time > witness.request_deadline,
        "{} should emit an exceeded deadline",
        witness.theorem_name
    );
    assert!(witness.deadline_exceeded);
    assert_eq!(
        witness.well_formed_source,
        "ComposedState.wellFormed_from_initial"
    );
    assert_eq!(witness.cancel_cause.as_deref(), Some("deadline"));
    assert!(
        witness
            .transition_path
            .iter()
            .any(|step| step == "slot_acquire"),
        "{} must include the composed admission grant",
        witness.theorem_name
    );
    assert!(
        witness
            .transition_path
            .iter()
            .any(|step| step == "clock_advance"),
        "{} must include the lockstep clock advance",
        witness.theorem_name
    );
}
