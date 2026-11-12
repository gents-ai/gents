use std::collections::BTreeSet;
use std::time::Duration;

use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};

use gents::defra_node::Node as EmbeddedNode;
use gents_automation::triggers::event_delivery::runtime_event_delivery_source_contracts;
use gents_inference::admission::InferenceCall;
use gents_inference::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use gents_session::request::event_delivery::EventDeliverySourceContract;
use gents_session::request::lifecycle::{ClaimOutcome, ExecutionOrigin, TriggerLineage};
use gents_session::{
    request::lifecycle::write_manual_agent_request, request::lifecycle::RequestLifecycle,
};
use gents_store::graphql::escape_graphql_string;
use serde::Deserialize;
use serde_json::{json, Value};

#[path = "../../gents-inference/src/admission/slot_accounting.rs"]
mod admission_slot_accounting;
mod support;

use admission_slot_accounting::{
    reconstructed_running_slot_count, slot_contribution, InferenceCallSlotRow,
};
use gents_lean_vocab::{
    assert_lean_transition_is_illegal, assert_lean_transition_is_legal,
    assert_state_machine_contract_is_complete,
    lean_client_agent_readiness_cases, lean_compaction_cursor_cases,
    lean_backend_health_cases, lean_command_env_case,
    lean_compaction_reducer_cases, lean_contract_snapshot, lean_event_delivery_convergence_traces,
    lean_event_delivery_source_instances, lean_event_delivery_transition_cases,
    lean_interrupt_disposition_cases, lean_mcp_health_cases,
    lean_inference_slot_accounting_cases, lean_interrupt_disposition_cases,
    lean_queue_deadline_cases, lean_r4c_background_work_cases,
    lean_r6_background_theorem_witnesses, lean_r6_backgrounding_case, lean_r6_backgrounding_cases,
    lean_recovery_sweep_cases, lean_request_transition_cases,
    lean_session_control_cases, lean_startup_readiness_cases, lean_tool_output_paging_cases,
    lean_vocabulary_values, LeanEventDeliveryAction,
    LeanLifecycleTransitionCase,
};
use support::snapshots::{
    fetch_request_lineage_snapshot, fetch_request_snapshot, fetch_request_snapshot_raw,
    fetch_session_snapshot, RequestLineageSnapshot, RequestSnapshot,
};
use support::{
    build_request, first_optional_row, first_row, test_db, BACKEND_ID, DEADLINE_SECS,
};

#[path = "conformance/background.rs"]
mod background;
#[path = "conformance/compaction_gate.rs"]
mod compaction_gate;
#[path = "conformance/coverage.rs"]
mod coverage;
#[path = "conformance/enrollment.rs"]
mod enrollment;
#[path = "conformance/eval.rs"]
mod eval;
#[path = "conformance/event_delivery.rs"]
mod event_delivery;
#[path = "conformance/goals.rs"]
mod goals;
#[path = "conformance/hooks.rs"]
mod hooks;
#[path = "conformance/interrupts_manual.rs"]
mod interrupts_manual;
#[path = "conformance/p2p_replication.rs"]
mod p2p_replication;
#[path = "conformance/plugin_job.rs"]
mod plugin_job;
#[path = "conformance/prompt_template.rs"]
mod prompt_template;
#[path = "conformance/recovery_sweeps.rs"]
mod recovery_sweeps;
#[path = "conformance/request_input.rs"]
mod request_input;
#[path = "conformance/request_lifecycle.rs"]
mod request_lifecycle;
#[path = "conformance/request_owner_scope.rs"]
mod request_owner_scope;
#[path = "conformance/session_control.rs"]
mod session_control;
#[path = "conformance/startup_readiness.rs"]
mod startup_readiness;
#[path = "conformance/streaming_compaction.rs"]
mod streaming_compaction;

#[tokio::test]
async fn generated_recovery_sweep_cases_drive_startup_recovery_contract() {
    recovery_sweeps::generated_recovery_sweep_cases_drive_startup_recovery_contract().await;
}

#[tokio::test]
async fn startup_recovery_order_terminalizes_crash_orphaned_calls() {
    recovery_sweeps::startup_recovery_order_terminalizes_crash_orphaned_calls().await;
}

#[tokio::test]
async fn single_claimer_watcher_never_claims_foreign_replica() {
    request_owner_scope::single_claimer_watcher_never_claims_foreign_replica().await;
}

#[tokio::test]
async fn canonical_recovery_repairs_expired_request_after_terminal_write_gap() {
    request_owner_scope::canonical_recovery_repairs_expired_request_after_terminal_write_gap()
        .await;
}

#[tokio::test]
async fn recover_stuck_requests_recovers_claimed_lifecycle_state() {
    request_owner_scope::recover_stuck_requests_recovers_claimed_lifecycle_state().await;
}

#[tokio::test]
async fn reconcile_coalesce_never_supersedes_foreign_replica() {
    request_owner_scope::reconcile_coalesce_never_supersedes_foreign_replica().await;
}

#[tokio::test]
async fn drain_wakeups_never_interrupts_foreign_replica() {
    request_owner_scope::drain_wakeups_never_interrupts_foreign_replica().await;
}

#[tokio::test]
async fn generated_r6_backgrounding_case_metadata_matches_export() {
    background::generated_r6_backgrounding_case_metadata_matches_export().await;
}

#[test]
fn generated_startup_readiness_cases_pin_bounded_barrier_release() {
    startup_readiness::generated_startup_readiness_cases_pin_bounded_barrier_release();
}

#[test]
fn generated_compaction_reducer_cases_pin_contract() {
    streaming_compaction::generated_compaction_reducer_cases_pin_contract();
}

#[test]
fn compaction_runtime_reduces_valid_canonical_history() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("build compaction-gate runtime")
        .block_on(compaction_gate::compaction_runtime_reduces_valid_canonical_history());
}

#[tokio::test]
async fn generated_queue_deadline_cases_pin_r4a_contract_rows() {
    request_lifecycle::generated_queue_deadline_cases_pin_r4a_contract_rows().await;
}

#[tokio::test]
async fn generated_request_transition_cases_cover_lifecycle_policy() {
    request_lifecycle::generated_request_transition_cases_cover_lifecycle_policy().await;
}

#[tokio::test]
async fn event_delivery_transition_cases_match_contract() {
    event_delivery::event_delivery_transition_cases_match_contract().await;
}

#[test]
fn event_delivery_source_instances_match_runtime() {
    event_delivery::event_delivery_source_instances_match_runtime();
}

#[tokio::test]
async fn event_delivery_convergence_traces_match_runtime_or_deviation() {
    event_delivery::event_delivery_convergence_traces_match_runtime_or_deviation().await;
}

#[test]
fn rust_eval_outcome_vocabulary_and_projection_match_lean() {
    eval::rust_eval_outcome_vocabulary_and_projection_match_lean();
}

#[path = "conformance/docs.rs"]
mod docs;
#[path = "conformance/identity.rs"]
mod identity;
#[path = "conformance/live_overlay.rs"]
mod live_overlay;
#[path = "conformance/manual_run.rs"]
mod manual_run;
#[path = "conformance/pairing_reconcile.rs"]
mod pairing_reconcile;
#[path = "conformance/prompt_assembly.rs"]
mod prompt_assembly;
#[path = "conformance/root_admission.rs"]
mod root_admission;
#[path = "conformance/scope_templates.rs"]
mod scope_templates;
#[path = "conformance/tool_policy.rs"]
mod tool_policy;
