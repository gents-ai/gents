import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Boundaries

/-!
# Conformance Coverage Ledger

Every domain emitted by `Proofs.Conformance.Contracts` must have a Rust
consumer or an explicitly accepted boundary/follow-up. Rust checks this ledger
against the generated JSON so new Lean contracts cannot remain advisory-only.
-/

namespace Conformance.Contracts

structure CoverageEntry where
  category : String
  domain : String
  consumer : String
  acceptedBoundary : String
  acceptedFollowUp : String
  deriving Repr

-- Consumer strings are registered Rust/TypeScript test pointers. The Rust
-- registry in `tests/support/conformance_consumers.rs` resolves each pointer
-- against the named source file and test, so stale consumer names fail tests.
def consumerCoverage
    (category domain consumer : String) : CoverageEntry :=
  { category := category
  , domain := domain
  , consumer := consumer
  , acceptedBoundary := ""
  , acceptedFollowUp := ""
  }

def boundaryCoverage
    (category domain acceptedBoundary : String)
    (consumer : String := "") : CoverageEntry :=
  { category := category
  , domain := domain
  , consumer := consumer
  , acceptedBoundary := acceptedBoundary
  , acceptedFollowUp := ""
  }

def followUpCoverage
    (category domain acceptedFollowUp : String) : CoverageEntry :=
  { category := category
  , domain := domain
  , consumer := ""
  , acceptedBoundary := ""
  , acceptedFollowUp := acceptedFollowUp
  }

def consumerWithFollowUpCoverage
    (category domain consumer acceptedFollowUp : String) : CoverageEntry :=
  { category := category
  , domain := domain
  , consumer := consumer
  , acceptedBoundary := ""
  , acceptedFollowUp := acceptedFollowUp
  }

def vocabularyCoverage : List CoverageEntry :=
  [ consumerCoverage
      "vocabulary"
      "RequestState"
      "lifecycle::tests::rust_request_lifecycle_state_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "ExecutionOrigin"
      "lifecycle::tests::rust_execution_origin_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "ProcessState"
      "runtime_status::tests::rust_process_state_vocabulary_matches_lean_model"
  , boundaryCoverage
      "vocabulary"
      "PersistenceState"
      boundaryPersistenceAbstractLifecycleId
  , boundaryCoverage
      "vocabulary"
      "PersistenceFailurePolicy"
      boundaryStorageHookFailurePolicyId
  , consumerCoverage
      "vocabulary"
      "ReconcilePhase"
      "runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model"
  , boundaryCoverage
      "vocabulary"
      "StorageObservation"
      boundaryStorageObservationDaemonVisibleId
  , consumerCoverage
      "vocabulary"
      "SessionRecoveryLatestRequestState"
      "state_machine_conformance::generated_session_recovery_cases_drive_db_backed_reissue_contract"
  , consumerCoverage
      "vocabulary"
      "InferenceCallState"
      "admission::tests::rust_inference_call_state_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "InferenceCallTerminalReason"
      "admission::tests::rust_inference_call_terminal_reason_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "ToolRetryDisposition"
      "mcp_pool::tests::tool_retry_disposition_contract_cases_match_mcp_pool_policy"
  , consumerCoverage
      "vocabulary"
      "ToolCallState"
      "tool_call_lifecycle::tests::rust_tool_call_state_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "ToolFailureClass"
      "tool_call_lifecycle::tests::rust_failure_class_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "AwaitMode"
      "state_machine_conformance::lean_emits_await_mode_vocabulary"
  , consumerCoverage
      "vocabulary"
      "CancelPolicy"
      "state_machine_conformance::lean_emits_cancel_policy_vocabulary"
  , consumerCoverage
      "vocabulary"
      "ChildTerminal"
      "state_machine_conformance::lean_emits_child_terminal_vocabulary_and_projections"
  ]

def stateMachineCoverage : List CoverageEntry :=
  [ consumerCoverage
      "state_machine"
      "Request"
      "lifecycle::tests::request_state_machine_contract_is_complete"
  , consumerCoverage
      "state_machine"
      "Process"
      "runtime_status::tests::rust_process_state_transitions_match_lean_contract"
  , boundaryCoverage
      "state_machine"
      "Persistence.failClosed"
      boundaryStorageHookFailurePolicyId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains"
  , boundaryCoverage
      "state_machine"
      "Persistence.failOpen"
      boundaryStorageHookFailurePolicyId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains"
  , boundaryCoverage
      "state_machine"
      "StorageObservation.failClosed"
      boundaryStorageObservationDaemonVisibleId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains"
  , boundaryCoverage
      "state_machine"
      "StorageObservation.failOpen"
      boundaryStorageObservationDaemonVisibleId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains"
  , consumerCoverage
      "state_machine"
      "RuntimeReconcile"
      "runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model"
  , consumerCoverage
      "state_machine"
      "PairingReconcile"
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains"
  , consumerCoverage
      "state_machine"
      "SessionRecovery"
      "state_machine_conformance::generated_session_recovery_cases_drive_db_backed_reissue_contract"
  , consumerCoverage
      "state_machine"
      "InferenceCall"
      "admission::tests::rust_inference_call_transition_table_matches_lean_contract"
  , consumerCoverage
      "state_machine"
      "ToolCall"
      "tool_call_lifecycle::tests::tool_call_state_machine_contract_is_complete"
  , consumerCoverage
      "state_machine"
      "AwaitMode"
      "state_machine_conformance::lean_emits_await_mode_vocabulary"
  , consumerCoverage
      "state_machine"
      "CancelPolicy"
      "state_machine_conformance::lean_emits_cancel_policy_vocabulary"
  , consumerCoverage
      "state_machine"
      "ChildTerminal"
      "state_machine_conformance::lean_emits_child_terminal_vocabulary_and_projections"
  ]

def caseCoverage : List CoverageEntry :=
  [ consumerCoverage
      "lifecycle_transition_cases"
      "RequestTransitions"
      "state_machine_conformance::generated_request_transition_cases_cover_lifecycle_policy"
  , consumerCoverage
      "lifecycle_transition_cases"
      "ProcessTransitions"
      "runtime_status::tests::generated_process_transition_cases_match_runtime_status_policy"
  , consumerCoverage
      "trigger_cases"
      "TriggerDispatch"
      "trigger_engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases"
  , consumerCoverage
      "runtime_cases"
      "RuntimeReconcileCases"
      "runtime_status::tests::runtime_status_generation_updates_match_lean_runtime_reconcile_cases"
  , consumerCoverage
      "apply_reconcile_cases"
      "ApplyReconcileCases"
      "apply_conformance::generated_apply_reconcile_cases_drive_apply_model_and_production_ordering"
  , consumerCoverage
      "session_recovery_cases"
      "SessionRecoveryCases"
      "state_machine_conformance::generated_session_recovery_cases_drive_db_backed_reissue_contract"
  , consumerCoverage
      "slot_cases"
      "InferenceCallSlotAccounting"
      "admission::tests::generated_inference_slot_accounting_cases_match_admission_reconstruction_logic"
  , boundaryCoverage
      "fleet_cases"
      "FleetSlotAccounting"
      boundaryFleetSlotAccountingDerivedViewId
      "admission::tests::generated_slot_accounting_fleet_cases_match_admission_runtime_boundary"
  , boundaryCoverage
      "persistence_policy_cases"
      "PersistenceFailurePolicyCases"
      boundaryStorageHookFailurePolicyId
      "hook::tests::generated_persistence_failure_policy_cases_match_hook_decisions"
  , boundaryCoverage
      "storage_observation_cases"
      "StorageObservationRuntimeCases"
      boundaryStorageObservationDaemonVisibleId
      "hook::tests::generated_storage_observation_cases_match_hook_runtime_classification"
  , boundaryCoverage
      "backend_health_cases"
      "BackendHealthAdmissionCases"
      boundaryBackendHealthAdmissionFreshnessId
      "backend_registry::tests::generated_backend_health_admission_cases_match_registry_and_admission_policy"
  , consumerCoverage
      "native_filesystem_boundary_cases"
      "NativeFilesystemBoundaryCases"
      "toolset::tests::generated_native_filesystem_boundary_cases_match_preemptible_boundary_contract"
  , consumerCoverage
      "frontend_client_shell_cases"
      "FrontendClientShellCases"
      "apps/desktop-tauri/src/lib/chat-shell.test.ts::projectChatShell matches generated Lean ClientShell projection contracts"
  , consumerCoverage
      "desktop_client_shell_cases"
      "DesktopClientShellCases"
      "defra_agent_desktop_tauri::bridge::snapshot::tests::session_state::session_snapshot_projection_consumes_generated_client_shell_contract_cases"
  , consumerCoverage
      "live_overlay_cases"
      "LiveOverlayCases"
      "live_overlay_conformance::live_overlay_cases_match_lean_table"
  , consumerCoverage
      "tool_cases"
      "ToolExecutionPreflight"
      "state_machine_conformance::generated_tool_execution_cases_cover_preflight_and_retry_contracts"
  , consumerCoverage
      "tool_cases"
      "ToolExecutionRetry"
      "mcp_pool::tests::tool_retry_disposition_contract_cases_match_mcp_pool_policy"
  , consumerCoverage
      "command_policy_cases"
      "CommandPolicyValidation"
      "toolset::tests::generated_command_policy_cases_match_rust_validation"
  , consumerCoverage
      "command_policy_cases"
      "CommandPolicySandbox"
      "toolset::tests::generated_command_sandbox_cases_match_rust_selection"
  , consumerCoverage
      "command_policy_cases"
      "CommandPolicyEnv"
      "toolset::tests::generated_command_env_cases_match_rust_filtering"
  , consumerWithFollowUpCoverage
      "queue_deadline_cases"
      "QueueDeadlineConformanceCases"
      "state_machine_conformance::generated_queue_deadline_cases_pin_r4a_contract_rows"
      "Runtime-backed queue/deadline consumers land in R4a Task 5 and Task 7 after the Rust claim and scheduler implementations exist."
  , consumerWithFollowUpCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases"
      "state_machine_conformance::generated_recovery_sweep_cases_pin_startup_recovery_contract"
      "Deadline audit PR E must implement InferenceCall::recover_all; the bridge terminal wiring follow-up must implement detached bridge row recovery."
  , consumerCoverage
      "r6_background_cases"
      "R6BackgroundingCases"
      "state_machine_conformance::generated_r6_backgrounding_cases_pin_tool_backgrounding_contract"
  , consumerCoverage
      "r4c_background_work_cases"
      "R4cBackgroundWorkCases"
      "state_machine_conformance::generated_r4c_background_work_cases_pin_observable_shapes"
  , consumerCoverage
      "transcript_cases"
      "TranscriptConformanceCases"
      "state_machine_conformance::generated_transcript_cases_pin_agent_message_ordering_contract"
  , consumerCoverage
      "identity_structural_cases"
      "IdentityStructuralCases"
      "identity_conformance::identity_structural_cases_match_lean_verdicts"
  , consumerWithFollowUpCoverage
      "identity_permission_cases"
      "IdentityPermissionCases"
      "identity_conformance::identity_permission_cases_pin_runtime_permission_contract_shape"
      "Issue #193 replaces the Rust mirror in identity_conformance::identity_permission_cases_pin_runtime_permission_contract_shape with the runtime permission decision module and deployment hostability lookup."
  , consumerCoverage
      "identity_contracts"
      "IdentityContracts"
      "identity_conformance::identity_respects_principal_contract_is_declared"
  , consumerWithFollowUpCoverage
      "streaming_response_cases"
      "ResponseTransitionCases"
      "state_machine_conformance::generated_streaming_response_cases_pin_lifecycle_contract"
      "Runtime-backed streaming response lifecycle drive remains a follow-up; this row pins the emitted Lean case shape."
  , consumerWithFollowUpCoverage
      "compaction_reducer_cases"
      "CompactionReducerCases"
      "state_machine_conformance::generated_compaction_reducer_cases_pin_contract"
      "Runtime-backed compaction reducer drive remains a follow-up; this row pins the emitted Lean case shape."
  , consumerCoverage
      "event_delivery_cases"
      "EventDeliveryTransitionCases"
      "state_machine_conformance::event_delivery_transition_cases_match_contract"
  , consumerCoverage
      "event_delivery_cases"
      "EventDeliverySourceInstances"
      "state_machine_conformance::event_delivery_source_instances_match_runtime"
  , consumerCoverage
      "event_delivery_cases"
      "EventDeliveryConvergenceTraces"
      "state_machine_conformance::event_delivery_convergence_traces_match_runtime_or_deviation"
  , consumerCoverage
      "mcp_health_cases"
      "MCPHealthCases"
      "health_checker::tests::generated_mcp_health_k1_cases_match_health_checker_transitions"
  ]

def followUpHookCoverage : List CoverageEntry :=
  []

def coverageLedger : List CoverageEntry :=
  vocabularyCoverage ++ stateMachineCoverage ++ caseCoverage ++ followUpHookCoverage

def CoverageEntry.toJson (entry : CoverageEntry) : String :=
  "{"
    ++ "\"category\":" ++ jsonString entry.category ++ ","
    ++ "\"domain\":" ++ jsonString entry.domain ++ ","
    ++ "\"consumer\":" ++ jsonString entry.consumer ++ ","
    ++ "\"accepted_boundary\":" ++ jsonString entry.acceptedBoundary ++ ","
    ++ "\"accepted_follow_up\":" ++ jsonString entry.acceptedFollowUp
    ++ "}"

def coverageLedgerJson : String :=
  jsonArray (coverageLedger.map CoverageEntry.toJson)

end Conformance.Contracts
