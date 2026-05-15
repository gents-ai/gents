use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::event_delivery_contract::{
    runtime_event_delivery_source_contracts, EventDeliverySourceContract,
};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::lifecycle::{ClaimOutcome, ExecutionOrigin, TriggerLineage};
use defra_agent::{
    write_manual_agent_request, DefraSessionHook, DefraStreamWriter, FailurePolicy,
    RequestLifecycle,
};
use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::one_or_many::OneOrMany;
use rig::streaming::StreamingCompletionResponse;
use serde::Deserialize;
use serde_json::json;

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
    lean_client_shell_case, lean_command_env_case, lean_command_policy_case,
    lean_command_sandbox_case, lean_compaction_reducer_case, lean_compaction_reducer_cases,
    lean_contract_snapshot, lean_event_delivery_convergence_traces,
    lean_event_delivery_source_instances, lean_event_delivery_transition_cases,
    lean_fleet_slot_accounting_case, lean_inference_slot_accounting_case, lean_mcp_health_cases,
    lean_queue_deadline_case, lean_queue_deadline_cases, lean_r4c_background_work_case,
    lean_r4c_background_work_cases, lean_r6_backgrounding_case, lean_r6_backgrounding_cases,
    lean_recovery_sweep_case, lean_recovery_sweep_cases, lean_request_transition_cases,
    lean_response_transition_case, lean_response_transition_cases, lean_runtime_reconcile_case,
    lean_session_recovery_case, lean_state_machine_contract, lean_tool_preflight_case,
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
    create_response_with_status, first_optional_row, set_interrupt_requested_at,
    set_request_lifecycle_state, set_valid_until, test_db, upsert_conversation, AGENT_DID,
    AGENT_NAME, BACKEND_ID, DEADLINE_SECS,
};

#[path = "state_machine_conformance/client_runtime.rs"]
mod client_runtime;
#[path = "state_machine_conformance/coverage.rs"]
mod coverage;
#[path = "state_machine_conformance/event_delivery.rs"]
mod event_delivery;
#[path = "state_machine_conformance/interrupts_manual.rs"]
mod interrupts_manual;
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

#[test]
fn generated_recovery_sweep_cases_pin_startup_recovery_contract() {
    recovery_sweeps::generated_recovery_sweep_cases_pin_startup_recovery_contract();
}

#[test]
fn generated_r6_backgrounding_cases_pin_tool_backgrounding_contract() {
    transcript_background::generated_r6_backgrounding_cases_pin_tool_backgrounding_contract();
}

#[test]
fn generated_r4c_background_work_cases_pin_observable_shapes() {
    transcript_background::generated_r4c_background_work_cases_pin_observable_shapes();
}

#[tokio::test]
async fn generated_transcript_cases_pin_agent_message_ordering_contract() {
    transcript_background::generated_transcript_cases_pin_agent_message_ordering_contract().await;
}

#[test]
fn generated_streaming_response_cases_pin_lifecycle_contract() {
    streaming_compaction::generated_streaming_response_cases_pin_lifecycle_contract();
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
fn generated_slot_accounting_cases_pin_inference_and_fleet_contracts() {
    tooling_slots_queue_command::generated_slot_accounting_cases_pin_inference_and_fleet_contracts(
    );
}

#[test]
fn generated_queue_deadline_cases_pin_r4a_contract_rows() {
    tooling_slots_queue_command::generated_queue_deadline_cases_pin_r4a_contract_rows();
}

#[tokio::test]
async fn generated_request_transition_cases_cover_lifecycle_policy() {
    request_lifecycle::generated_request_transition_cases_cover_lifecycle_policy().await;
}

#[test]
fn event_delivery_transition_cases_match_contract() {
    event_delivery::event_delivery_transition_cases_match_contract();
}

#[test]
fn event_delivery_source_instances_match_runtime() {
    event_delivery::event_delivery_source_instances_match_runtime();
}

#[test]
fn event_delivery_convergence_traces_match_runtime_or_deviation() {
    event_delivery::event_delivery_convergence_traces_match_runtime_or_deviation();
}
