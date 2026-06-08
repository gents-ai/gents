use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::event_delivery_contract::{
    runtime_event_delivery_source_contracts, EventDeliverySourceContract,
};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::lifecycle::{ClaimOutcome, ExecutionOrigin, TriggerLineage};
use defra_agent::tool_call_lifecycle::{
    AwaitMode, CancelCause, CancelPolicy, CascadeDispatch, ToolCallLifecycle, MAX_SUBAGENT_DEPTH,
};
use defra_agent::{
    fetch_interrupt_requested_at, interrupt_request, upsert_agent_behavior, upsert_tool_selection,
    write_manual_agent_request, AgentBehaviorDocument, BackgroundToolRegistry, DefraSessionHook,
    DefraStreamWriter, FailurePolicy, InferenceCall, RequestLifecycle, ToolSelectionDocument,
};
use rig::agent::{HookAction, ToolCallHookAction};
use rig::completion::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use rig::completion::ToolDefinition;
use rig::one_or_many::OneOrMany;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
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
    lean_client_shell_case, lean_codex_shim_projection_case, lean_codex_shim_projection_cases,
    lean_codex_shim_turn_lifecycle_cases, lean_command_env_case, lean_command_policy_case,
    lean_command_sandbox_case, lean_compaction_reducer_cases, lean_contract_snapshot,
    lean_event_delivery_convergence_traces, lean_event_delivery_source_instances,
    lean_event_delivery_transition_cases, lean_fleet_slot_accounting_case,
    lean_inference_slot_accounting_case, lean_managed_exec_liveness_cases, lean_mcp_health_cases,
    lean_queue_deadline_case, lean_queue_deadline_cases, lean_r4c_background_work_case,
    lean_r4c_background_work_cases, lean_r5_cross_deployment_cases,
    lean_r6_background_theorem_witness, lean_r6_background_theorem_witnesses,
    lean_r6_backgrounding_case, lean_r6_backgrounding_cases, lean_recovery_equivalence_cases,
    lean_recovery_sweep_cases, lean_request_transition_cases, lean_response_interrupt_flow_cases,
    lean_response_transition_cases, lean_runtime_reconcile_case, lean_session_recovery_case,
    lean_state_machine_contract, lean_subagent_delegation_graph_cases, lean_tool_preflight_case,
    lean_tool_retry_case, lean_transcript_case, lean_transcript_cases, lean_vocabulary_values,
    LeanEventDeliveryAction, LeanLifecycleTransitionCase, LeanR4cBackgroundWorkCase,
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

#[path = "state_machine_conformance/client_runtime.rs"]
mod client_runtime;
#[path = "state_machine_conformance/codex_shim.rs"]
mod codex_shim;
#[path = "state_machine_conformance/coverage.rs"]
mod coverage;
#[path = "state_machine_conformance/event_delivery.rs"]
mod event_delivery;
#[path = "state_machine_conformance/interrupts_manual.rs"]
mod interrupts_manual;
#[path = "state_machine_conformance/r5_cross_deployment.rs"]
mod r5_cross_deployment;
#[path = "state_machine_conformance/recovery_sweeps.rs"]
mod recovery_sweeps;
#[path = "state_machine_conformance/request_lifecycle.rs"]
mod request_lifecycle;
#[path = "state_machine_conformance/session_recovery.rs"]
mod session_recovery;
#[path = "state_machine_conformance/streaming_compaction.rs"]
mod streaming_compaction;
#[path = "state_machine_conformance/tool_call.rs"]
mod tool_call;
#[path = "state_machine_conformance/tooling_slots_queue_command.rs"]
mod tooling_slots_queue_command;
#[path = "state_machine_conformance/transcript_background.rs"]
mod transcript_background;

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

#[test]
fn generated_r6_backgrounding_cases_pin_tool_backgrounding_contract() {
    transcript_background::generated_r6_backgrounding_cases_pin_tool_backgrounding_contract();
}

#[tokio::test]
async fn generated_r6_background_theorem_witnesses_drive_admission_budget_invariant() {
    transcript_background::generated_r6_background_theorem_witnesses_drive_admission_budget_invariant()
        .await;
}

#[tokio::test]
async fn generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace() {
    transcript_background::generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace()
        .await;
}

#[test]
fn generated_subagent_delegation_graph_cases_pin_gap2_contract() {
    transcript_background::generated_subagent_delegation_graph_cases_pin_gap2_contract();
}

#[tokio::test]
async fn generated_r5_cross_deployment_cases_drive_production_dispatch() {
    r5_cross_deployment::generated_r5_cross_deployment_cases_drive_production_dispatch().await;
}

#[test]
fn generated_r4c_background_work_cases_pin_observable_shapes() {
    transcript_background::generated_r4c_background_work_cases_pin_observable_shapes();
}

#[test]
fn generated_codex_shim_projection_cases_pin_adapter_mapping() {
    codex_shim::generated_codex_shim_projection_cases_pin_adapter_mapping();
}

#[tokio::test]
async fn generated_transcript_cases_drive_agent_message_ordering_contract() {
    transcript_background::generated_transcript_cases_drive_agent_message_ordering_contract().await;
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
    tooling_slots_queue_command::generated_tool_execution_cases_cover_preflight_and_retry_contracts(
    );
}

#[test]
fn managed_exec_liveness_cases_pin_native_process_boundary() {
    let machine = lean_state_machine_contract("ManagedExec");
    assert_eq!(
        machine.states,
        vec![
            "pendingSpawn",
            "running",
            "exited",
            "killSignaled",
            "killed",
            "spawnFailed",
            "reapFailed"
        ]
    );
    assert!(machine
        .legal_transitions
        .iter()
        .any(|pair| pair.from == "running" && pair.to == "killSignaled"));
    assert!(machine
        .legal_transitions
        .iter()
        .any(|pair| pair.from == "killSignaled" && pair.to == "killed"));

    let cases = lean_managed_exec_liveness_cases();
    assert_eq!(cases.len(), 5);
    let deadline = cases
        .iter()
        .find(|case| case.name == "running_child_expired_deadline_kill_signaled")
        .expect("deadline liveness case must be emitted");
    assert_eq!(deadline.trigger, "deadlineElapsed");
    assert_eq!(deadline.pre_exec_state, "running");
    assert_eq!(deadline.pre_tool_state, "running");
    assert_eq!(deadline.expected_exec_state, "killSignaled");
    assert_eq!(deadline.expected_tool_state, "timedOut");
    assert_eq!(deadline.max_steps, 1);
    assert!(deadline.kill_signal_required);

    let cancel = cases
        .iter()
        .find(|case| case.name == "running_child_cancel_kill_signaled")
        .expect("cancel liveness case must be emitted");
    assert_eq!(cancel.trigger, "cancelRequested");
    assert_eq!(cancel.expected_tool_state, "cancelled");
    assert!(cancel.kill_signal_required);

    for case in cases {
        if case.expected_exec_state == "killSignaled" {
            assert!(
                case.kill_signal_required,
                "kill-signaled cases must require an OS signal: {case:?}"
            );
        } else {
            assert!(
                !case.kill_signal_required,
                "non-kill cases must not require an OS signal: {case:?}"
            );
        }
    }
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
    tooling_slots_queue_command::generated_slot_accounting_cases_pin_inference_and_fleet_contracts(
    );
}

#[tokio::test]
async fn generated_queue_deadline_cases_pin_r4a_contract_rows() {
    tooling_slots_queue_command::generated_queue_deadline_cases_pin_r4a_contract_rows().await;
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
