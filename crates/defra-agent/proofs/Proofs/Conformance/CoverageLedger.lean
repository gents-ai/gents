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
      "CancelCause"
      "tool_call_lifecycle::tests::rust_cancel_cause_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "ManagedExecState"
      "managed_exec::tests::rust_managed_exec_state_vocabulary_matches_lean_model"
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
      "runtime_status::tests::runtime_reconcile_state_machine_contract_is_complete"
  , consumerCoverage
      "state_machine"
      "PairingReconcile"
      "agent::reconcile::tests::pairing_reconcile_state_machine_contract_is_complete"
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
      "ManagedExec"
      "managed_exec::tests::managed_exec_state_machine_contract_is_complete"
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
      "config_import::lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary"
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
      "managed_exec_cases"
      "ManagedExecLivenessCases"
      "state_machine_conformance::managed_exec_liveness_cases_pin_native_process_boundary"
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
  , consumerCoverage
      "queue_deadline_cases"
      "QueueDeadlineConformanceCases"
      "state_machine_conformance::generated_queue_deadline_cases_pin_r4a_contract_rows"
  , consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases"
      "state_machine_conformance::generated_recovery_sweep_cases_drive_startup_recovery_contract"
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
      "state_machine_conformance::generated_transcript_cases_drive_agent_message_ordering_contract"
  , consumerCoverage
      "identity_structural_cases"
      "IdentityStructuralCases"
      "identity_conformance::identity_structural_cases_match_lean_verdicts"
  , consumerCoverage
      "identity_permission_cases"
      "IdentityPermissionCases"
      "identity_conformance::identity_permission_cases_pin_runtime_permission_contract_shape"
  , consumerCoverage
      "identity_contracts"
      "IdentityContracts"
      "identity_conformance::identity_respects_principal_contract_enforced_by_runtime_routing"
  , consumerCoverage
      "streaming_response_cases"
      "ResponseTransitionCases"
      "state_machine_conformance::generated_streaming_response_cases_pin_lifecycle_contract"
  , consumerCoverage
      "compaction_reducer_cases"
      "CompactionReducerCases"
      "state_machine_conformance::generated_compaction_reducer_cases_pin_contract"
  -- See docs/superpowers/audits/2026-05-19-conformance-audit.md#15-eventdelivery
  -- and follow-up issue #252: this consumer still drives the Lean rows
  -- through InMemoryEventDeliverySource rather than the production
  -- DefraWatcher/EventSource/SubagentSource loops.
  , consumerWithFollowUpCoverage
      "event_delivery_cases"
      "EventDeliveryTransitionCases"
      "state_machine_conformance::event_delivery_transition_cases_match_contract"
      "Issue #252 replaces the InMemoryEventDeliverySource replay with a thin driver over the production DefraWatcher/EventSource/SubagentSource event-delivery loops."
  , consumerCoverage
      "event_delivery_cases"
      "EventDeliverySourceInstances"
      "state_machine_conformance::event_delivery_source_instances_match_runtime"
  -- See docs/superpowers/audits/2026-05-19-conformance-audit.md#15-eventdelivery
  -- and follow-up issue #252: this consumer still drives the Lean traces
  -- through InMemoryEventDeliverySource rather than the production
  -- DefraWatcher/EventSource/SubagentSource loops.
  , consumerWithFollowUpCoverage
      "event_delivery_cases"
      "EventDeliveryConvergenceTraces"
      "state_machine_conformance::event_delivery_convergence_traces_match_runtime_or_deviation"
      "Issue #252 replaces the InMemoryEventDeliverySource replay with a thin driver over the production DefraWatcher/EventSource/SubagentSource event-delivery loops."
  -- 2026-05-19 conformance audit section 10 / section 6 item #2:
  -- Stage 1 drives the K=1 runtime health-check path; Stage 2 issue #253
  -- must add K>=2 backoff behavior and drop the lean_mcp_health_k1_cases filter.
  , consumerWithFollowUpCoverage
      "mcp_health_cases"
      "MCPHealthCases"
      "health_checker::tests::generated_mcp_health_k1_cases_match_health_checker_transitions"
      "Issue #253 adds K>=2 MCPHealth backoff behavior in Rust and drops the lean_mcp_health_k1_cases() filter so the full emitted mcp_health_cases domain is consumed."
  ]

def followUpHookCoverage : List CoverageEntry :=
  [ followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.backgrounded_budget_bounded"
      "Subagent.BridgedState.backgrounded_budget_bounded proves that reachable bridged states keep live backgrounded tools at or below maxBackgroundedPerParent. Follow-up: emit witness row via `theorem_witness` discriminator in `Proofs/Conformance/ContractCases/R6Background.lean`."
  , followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.cascade_cancels_child"
      "Subagent.BridgedState.cascade_cancels_child proves cascade parent termination interrupts a linked processing child; related Lean-only negative form: Subagent.BridgedState.detach_does_not_cancel_child. Follow-up: emit witness row via `theorem_witness` discriminator in `Proofs/Conformance/ContractCases/R6Background.lean`."
  , followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.foreground_blocks_parent_advance"
      "Subagent.BridgedState.foreground_blocks_parent_advance proves live foreground tools block parent progress/message advance; related aliases: Subagent.BridgedState.subagent_depth_bounded and Subagent.BridgedState.bridge_link_symmetric. Accepted Lean-only today because the invariant is a proof-layer bridge guard rather than an emitted runtime witness."
  , followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.bridged_child_completion_propagates"
      "Subagent.BridgedState.bridged_child_completion_propagates proves child completion projects to parent bridge-tool completion; related failure projection: Subagent.BridgedState.bridged_child_failure_projects. Accepted Lean-only today because R6Background emits data-shape cases and this theorem remains a formal trace projection."
  , followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.inv_depth"
      "Subagent.BridgedState.inv_depth proves bridged traces preserve max subagent depth; related link invariant: Subagent.BridgedState.inv_link. Accepted Lean-only today because these are structural trace invariants with no current Rust witness surface."
  , followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.bridgedUniqueCallIds_preserved"
      "Subagent.BridgedState.bridgedUniqueCallIds_preserved proves parent and child tool call ids remain unique across bridged traces. Accepted Lean-only today because the theorem lifts a structural uniqueness proof rather than an operational R6 witness."
  ]

def followUpHookIds : List String :=
  followUpHookCoverage.map (fun entry => entry.domain)

def followUpHooksJson : String :=
  jsonArray (followUpHookIds.map jsonString)

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
