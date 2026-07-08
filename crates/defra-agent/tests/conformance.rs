use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::event_delivery_contract::{
    runtime_event_delivery_source_contracts, EventDeliverySourceContract,
};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::lifecycle::{ClaimOutcome, ExecutionOrigin, TriggerLineage};
use defra_agent::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use defra_agent::llm::tool::BoxFuture;
use defra_agent::llm::tool::ToolDefinition;
use defra_agent::llm::tool::{ToolDyn, ToolError};
use defra_agent::llm::{HookAction, ToolCallHookAction};
use defra_agent::tool_call_lifecycle::{
    AwaitMode, CancelCause, CancelPolicy, CascadeDispatch, ToolCallLifecycle, MAX_SUBAGENT_DEPTH,
};
use defra_agent::{
    fetch_interrupt_requested_at, interrupt_request, upsert_agent_behavior, upsert_tool_selection,
    write_manual_agent_request, AgentBehaviorDocument, BackgroundToolRegistry, DefraSessionHook,
    DefraStreamWriter, FailurePolicy, InferenceCall, RequestLifecycle, ToolSelectionDocument,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[path = "../src/admission/slot_accounting.rs"]
mod admission_slot_accounting;
#[path = "../src/lean_vocab_test.rs"]
mod lean_vocab_test;
mod support;

use admission_slot_accounting::{
    reconstructed_running_slot_count, slot_contribution, InferenceCallSlotRow,
};
use lean_vocab_test::{
    assert_lean_transition_is_illegal, assert_lean_transition_is_legal,
    assert_lifecycle_transition_cases_partition, assert_state_machine_contract_is_complete,
    lean_backend_health_cases, lean_cancel_propagation_cases, lean_client_shell_case,
    lean_codex_shim_projection_case, lean_codex_shim_projection_cases,
    lean_codex_shim_turn_lifecycle_cases, lean_command_env_case, lean_command_policy_case,
    lean_command_sandbox_case, lean_compaction_reducer_cases, lean_composed_invariant_witnesses,
    lean_contract_snapshot, lean_event_delivery_convergence_traces,
    lean_event_delivery_source_instances, lean_event_delivery_transition_cases,
    lean_fleet_slot_accounting_case, lean_inference_slot_accounting_case,
    lean_inference_slot_accounting_cases, lean_managed_exec_liveness_cases, lean_mcp_health_cases,
    lean_process_transition_cases, lean_queue_deadline_case, lean_queue_deadline_cases,
    lean_r4c_background_work_case, lean_r4c_background_work_cases, lean_r5_cross_deployment_cases,
    lean_r6_background_theorem_witness, lean_r6_background_theorem_witnesses,
    lean_r6_backgrounding_case, lean_r6_backgrounding_cases, lean_recovery_equivalence_cases,
    lean_recovery_sweep_cases, lean_request_transition_cases, lean_response_interrupt_flow_cases,
    lean_response_transition_cases, lean_runtime_reconcile_case, lean_runtime_reconcile_cases,
    lean_session_recovery_case, lean_state_machine_contract, lean_subagent_delegation_graph_cases,
    lean_transcript_case, lean_transcript_cases, lean_vocabulary_values, LeanEventDeliveryAction,
    LeanLifecycleTransitionCase, LeanR4cBackgroundWorkCase,
};
use support::conformance_consumers::assert_registered_conformance_consumers_resolve;
use support::snapshots::{
    fetch_conversation_snapshot, fetch_message_snapshots_for_session,
    fetch_request_lineage_snapshot, fetch_request_lineage_snapshot_by_tuple,
    fetch_request_snapshot, fetch_request_snapshot_raw, fetch_response_content,
    fetch_response_interrupted_at, fetch_response_snapshot, fetch_session_snapshot,
    fetch_tool_call_snapshots_for_session, ConversationSnapshot, MessageSnapshot,
    RequestLineageSnapshot, RequestSnapshot, ResponseSnapshot, SessionSnapshot, ToolCallSnapshot,
};
use support::{
    build_request, create_agent_session, create_request, create_response_with_content_and_status,
    create_response_with_status, first_optional_row, first_row, set_interrupt_requested_at,
    set_request_lifecycle_state, set_valid_until, test_db, upsert_conversation, AGENT_DID,
    AGENT_NAME, BACKEND_ID, DEADLINE_SECS,
};

#[path = "conformance/backend_health.rs"]
mod backend_health;
#[path = "conformance/background.rs"]
mod background;
#[path = "conformance/cancel_propagation.rs"]
mod cancel_propagation;
#[path = "conformance/client_runtime.rs"]
mod client_runtime;
#[path = "conformance/codex_shim.rs"]
mod codex_shim;
#[path = "conformance/command_policy.rs"]
mod command_policy;
#[path = "conformance/completion_retry.rs"]
mod completion_retry;
#[path = "conformance/composed_invariants.rs"]
mod composed_invariants;
#[path = "conformance/coverage.rs"]
mod coverage;
#[path = "conformance/event_delivery.rs"]
mod event_delivery;
#[path = "conformance/fleet.rs"]
mod fleet;
#[path = "conformance/inference_call.rs"]
mod inference_call;
#[path = "conformance/interrupts_manual.rs"]
mod interrupts_manual;
#[path = "conformance/managed_exec.rs"]
mod managed_exec;
#[path = "conformance/mcp_health.rs"]
mod mcp_health;
#[path = "conformance/process.rs"]
mod process;
#[path = "conformance/prompt_template.rs"]
mod prompt_template;
#[path = "conformance/r5_cross_deployment.rs"]
mod r5_cross_deployment;
#[path = "conformance/recovery_sweeps.rs"]
mod recovery_sweeps;
#[path = "conformance/request_lifecycle.rs"]
mod request_lifecycle;
#[path = "conformance/session_recovery.rs"]
mod session_recovery;
#[path = "conformance/streaming_compaction.rs"]
mod streaming_compaction;
#[path = "conformance/tool_call.rs"]
mod tool_call;
#[path = "conformance/transcript.rs"]
mod transcript;
#[path = "conformance/workflow_barrier.rs"]
mod workflow_barrier;

#[test]
fn lean_executable_contracts_cover_initial_domains() {
    coverage::lean_executable_contracts_cover_initial_domains();
}

#[tokio::test]
async fn generated_recovery_sweep_cases_drive_startup_recovery_contract() {
    recovery_sweeps::generated_recovery_sweep_cases_drive_startup_recovery_contract().await;
}

#[test]
fn generated_recovery_equivalence_cases_pin_uninterrupted_convergence_contract() {
    recovery_sweeps::generated_recovery_equivalence_cases_pin_uninterrupted_convergence_contract();
}

#[tokio::test]
async fn subagent_liveness_reconciliation_converges_expired_processing_to_zero() {
    recovery_sweeps::subagent_liveness_reconciliation_converges_expired_processing_to_zero().await;
}

#[test]
fn generated_r6_backgrounding_cases_pin_tool_backgrounding_contract() {
    background::generated_r6_backgrounding_cases_pin_tool_backgrounding_contract();
}

#[tokio::test]
async fn generated_r6_background_theorem_witnesses_drive_admission_budget_invariant() {
    background::generated_r6_background_theorem_witnesses_drive_admission_budget_invariant().await;
}

#[tokio::test]
async fn generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace() {
    background::generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace().await;
}

#[test]
fn generated_subagent_delegation_graph_cases_pin_gap2_contract() {
    background::generated_subagent_delegation_graph_cases_pin_gap2_contract();
}

#[tokio::test]
async fn generated_r5_cross_deployment_cases_drive_production_dispatch() {
    r5_cross_deployment::generated_r5_cross_deployment_cases_drive_production_dispatch().await;
}

#[tokio::test]
async fn generated_composed_invariant_witnesses_drive_tool_lifecycle_conformance() {
    composed_invariants::generated_composed_invariant_witnesses_drive_tool_lifecycle_conformance()
        .await;
}

#[tokio::test]
async fn cancel_propagation_cases_drive_production_interrupt() {
    cancel_propagation::cancel_propagation_cases_drive_production_interrupt().await;
}

#[test]
fn generated_r4c_background_work_cases_pin_observable_shapes() {
    background::generated_r4c_background_work_cases_pin_observable_shapes();
}

#[test]
fn generated_codex_shim_projection_cases_pin_adapter_mapping() {
    codex_shim::generated_codex_shim_projection_cases_pin_adapter_mapping();
}

#[tokio::test]
async fn generated_transcript_cases_drive_agent_message_ordering_contract() {
    transcript::generated_transcript_cases_drive_agent_message_ordering_contract().await;
}

#[tokio::test]
async fn generated_streaming_response_cases_pin_lifecycle_contract() {
    streaming_compaction::generated_streaming_response_cases_pin_lifecycle_contract().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_streaming_response_interrupt_flow_cases_drive_daemon_contract() {
    streaming_compaction::generated_streaming_response_interrupt_flow_cases_drive_daemon_contract()
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn generated_streaming_response_idle_timeout_case_drives_daemon_contract() {
    streaming_compaction::generated_streaming_response_idle_timeout_case_drives_daemon_contract()
        .await;
}

#[test]
fn generated_compaction_reducer_cases_pin_contract() {
    streaming_compaction::generated_compaction_reducer_cases_pin_contract();
}

#[tokio::test]
async fn generated_session_recovery_cases_drive_db_backed_reissue_contract() {
    session_recovery::generated_session_recovery_cases_drive_db_backed_reissue_contract().await;
}

#[test]
fn generated_tool_execution_cases_cover_preflight_and_retry_contracts() {
    tool_execution::generated_tool_execution_cases_cover_preflight_and_retry_contracts();
}

#[test]
fn generated_tool_policy_cases_match_lean_composition() {
    tool_policy::generated_tool_policy_cases_match_lean_composition();
}

#[test]
fn completion_retry_lean_witness_cases_hold() {
    completion_retry::completion_retry_lean_witness_cases_hold();
}

#[test]
fn managed_exec_liveness_cases_pin_native_process_boundary() {
    managed_exec::managed_exec_liveness_cases_pin_native_process_boundary();
}

#[test]
fn generated_mcp_health_cases_pin_threshold_projection_shape() {
    mcp_health::generated_mcp_health_cases_pin_threshold_projection_shape();
}

#[test]
fn generated_backend_health_cases_pin_threshold_and_veto_shape() {
    backend_health::generated_backend_health_cases_pin_threshold_and_veto_shape();
}

#[test]
fn generated_process_transition_cases_cover_runtime_status_policy_shape() {
    process::generated_process_transition_cases_cover_runtime_status_policy_shape();
}

#[tokio::test]
async fn generated_inference_slot_accounting_cases_drive_db_backed_reconstruction() {
    inference_call::generated_inference_slot_accounting_cases_drive_db_backed_reconstruction()
        .await;
}

#[test]
fn lean_emits_await_mode_vocabulary() {
    tool_call::lean_emits_await_mode_vocabulary();
}

#[test]
fn lean_emits_cancel_policy_vocabulary() {
    tool_call::lean_emits_cancel_policy_vocabulary();
}

#[test]
fn lean_emits_child_terminal_vocabulary_and_projections() {
    tool_call::lean_emits_child_terminal_vocabulary_and_projections();
}

#[test]
fn lean_tool_call_cancel_actions_name_cancel_cause() {
    tool_call::lean_tool_call_cancel_actions_name_cancel_cause();
}

#[test]
fn generated_slot_accounting_cases_pin_inference_and_fleet_contracts() {
    fleet::generated_slot_accounting_cases_pin_inference_and_fleet_contracts();
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

// ===== Absorbed standalone conformance binaries (one binary, mirrors Proofs/) =====
#[path = "conformance/apply_reconcile.rs"]
mod apply_reconcile;
#[path = "conformance/docs.rs"]
mod docs;
#[path = "conformance/identity.rs"]
mod identity;
#[path = "conformance/identity_proptest.rs"]
mod identity_proptest;
#[path = "conformance/live_overlay.rs"]
mod live_overlay;
#[path = "conformance/manual_run.rs"]
mod manual_run;
#[path = "conformance/pairing_reconcile.rs"]
mod pairing_reconcile;
#[path = "conformance/peer_registry_discovery.rs"]
mod peer_registry_discovery;
#[path = "conformance/prompt_assembly.rs"]
mod prompt_assembly;
#[path = "conformance/r5_scenarios.rs"]
mod r5_scenarios;
#[path = "conformance/scheduling.rs"]
mod scheduling;
#[path = "conformance/scope_templates.rs"]
mod scope_templates;
#[path = "conformance/structure.rs"]
mod structure;
#[path = "conformance/subagent_source.rs"]
mod subagent_source;
#[path = "conformance/tool_execution.rs"]
mod tool_execution;
#[path = "conformance/tool_execution_subagent.rs"]
mod tool_execution_subagent;
#[path = "conformance/tool_policy.rs"]
mod tool_policy;
#[path = "conformance/triggers.rs"]
mod triggers;
