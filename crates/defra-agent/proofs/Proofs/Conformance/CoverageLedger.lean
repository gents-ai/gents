import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Boundaries

/-!
# Conformance Coverage Ledger

Every domain emitted by `Proofs.Conformance.Contracts` must have a Rust
consumer or an explicitly accepted boundary/follow-up. Rust checks this ledger
against the generated JSON so new Lean contracts cannot remain advisory-only.
-/

namespace Conformance.Contracts

inductive Surface where
  | agentFacing
  | operatorCli
  | operatorUi
  | api
  | runtimeInternal
  deriving Repr, DecidableEq

def Surface.toString : Surface → String
  | Surface.agentFacing => "agentFacing"
  | Surface.operatorCli => "operatorCli"
  | Surface.operatorUi => "operatorUi"
  | Surface.api => "api"
  | Surface.runtimeInternal => "runtimeInternal"

def Surface.toJson (surface : Surface) : String :=
  jsonString surface.toString

def surfacesJson (surfaces : List Surface) : String :=
  jsonArray (surfaces.map Surface.toJson)

def allSurfaces : List Surface :=
  [ Surface.agentFacing
  , Surface.operatorCli
  , Surface.operatorUi
  , Surface.api
  , Surface.runtimeInternal
  ]

structure CoverageEntry where
  category : String
  domain : String
  consumer : String
  acceptedBoundary : String
  acceptedFollowUp : String
  feature : String := ""
  surfaces : List Surface := []
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

def tagged (entry : CoverageEntry)
    (feature : String) (surfaces : List Surface) : CoverageEntry :=
  { entry with feature := feature, surfaces := surfaces }

structure FeatureSurfaceRequirement where
  feature : String
  required : List Surface
  deferred : List (Surface × String)
  deriving Repr

def featureSurfaceDeferralJson (deferred : Surface × String) : String :=
  "{"
    ++ "\"surface\":" ++ Surface.toJson deferred.1 ++ ","
    ++ "\"note\":" ++ jsonString deferred.2
    ++ "}"

def FeatureSurfaceRequirement.toJson (req : FeatureSurfaceRequirement) : String :=
  "{"
    ++ "\"feature\":" ++ jsonString req.feature ++ ","
    ++ "\"required\":" ++ surfacesJson req.required ++ ","
    ++ "\"deferred\":" ++ jsonArray (req.deferred.map featureSurfaceDeferralJson)
    ++ "}"

def featureSurfaceRequirements : List FeatureSurfaceRequirement :=
  [ { feature := "request-lifecycle"
    , required := [Surface.agentFacing, Surface.runtimeInternal, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "process-lifecycle"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "inference-call"
    , required := [Surface.agentFacing, Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "tool-call"
    , required := [Surface.agentFacing, Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "managed-exec"
    , required := [Surface.agentFacing]
    , deferred := []
    }
  , { feature := "pairing-reconcile"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "runtime-reconcile"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "session-recovery"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "background-tools"
    , required := [Surface.agentFacing, Surface.operatorUi]
    , deferred :=
        [ (Surface.operatorCli, "#268")
        ]
    }
  , { feature := "subagents-cross-deployment"
    , required := [Surface.agentFacing, Surface.api, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "interrupt-and-cancel"
    , required := [Surface.agentFacing, Surface.operatorUi]
    , deferred :=
        [ (Surface.operatorCli, "#266")
        ]
    }
  , { feature := "mcp-health"
    , required := [Surface.runtimeInternal, Surface.operatorCli, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "identity-permission"
    , required := [Surface.runtimeInternal, Surface.api]
    , deferred := []
    }
  , { feature := "apply-reconcile"
    , required := [Surface.operatorCli]
    , deferred := [(Surface.operatorUi, "#281")]
    }
  , { feature := "event-delivery"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "triggers"
    , required := [Surface.runtimeInternal, Surface.operatorCli, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "compaction"
    , required := [Surface.agentFacing]
    , deferred := []
    }
  , { feature := "transcript"
    , required := [Surface.agentFacing]
    , deferred := [(Surface.operatorUi, "#284")]
    }
  , { feature := "streaming-response"
    , required := [Surface.agentFacing, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "client-shell"
    , required := [Surface.operatorUi]
    , deferred := []
    }
  , { feature := "codex-shim"
    , required := [Surface.api, Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "command-policy"
    , required := [Surface.agentFacing]
    -- #286 ships the inline denial render in the desktop chat shell,
    -- but the render is currently bound by regex-parsing the
    -- AgentToolCall.result bail!-string in
    -- apps/desktop-tauri/src/lib/commandDenial.ts. The runtime does
    -- not yet emit structured DenialReason fields, so there is no
    -- Lean-emitted case set the operator-UI consumer can bind to.
    -- The slot stays deferred until Path A (#329) lands: structured
    -- DenialReason on AgentToolCall + a Lean
    -- CommandPolicyOperatorUiCases that the desktop consumer can
    -- bind.
    , deferred := [(Surface.operatorUi, "#329")]
    }
  , { feature := "recovery"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "fleet-slot-accounting"
    , required := [Surface.runtimeInternal, Surface.api]
    , deferred := []
    }
  , { feature := "storage-observation"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "persistence-failure-policy"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "backend-health"
    , required := [Surface.runtimeInternal, Surface.operatorUi]
    , deferred := []
    }
  ]

def vocabularyCoverage : List CoverageEntry :=
  [ tagged (consumerCoverage
      "vocabulary"
      "RequestState"
      "lifecycle::tests::rust_request_lifecycle_state_vocabulary_matches_lean_model")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "ExecutionOrigin"
      "lifecycle::tests::rust_execution_origin_vocabulary_matches_lean_model")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "ProcessState"
      "runtime_status::tests::rust_process_state_vocabulary_matches_lean_model")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "vocabulary"
      "PersistenceState"
      boundaryPersistenceAbstractLifecycleId)
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "vocabulary"
      "PersistenceFailurePolicy"
      boundaryStorageHookFailurePolicyId)
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "ReconcilePhase"
      "runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "vocabulary"
      "StorageObservation"
      boundaryStorageObservationDaemonVisibleId)
      "storage-observation" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "SessionRecoveryLatestRequestState"
      "state_machine_conformance::generated_session_recovery_cases_drive_db_backed_reissue_contract")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "InferenceCallState"
      "admission::tests::rust_inference_call_state_vocabulary_matches_lean_model")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "InferenceCallTerminalReason"
      "admission::tests::rust_inference_call_terminal_reason_vocabulary_matches_lean_model")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "ToolRetryDisposition"
      "mcp_pool::tests::tool_retry_disposition_contract_cases_match_mcp_pool_policy")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "ToolCallState"
      "tool_call_lifecycle::tests::rust_tool_call_state_vocabulary_matches_lean_model")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "CancelCause"
      "tool_call_lifecycle::tests::rust_cancel_cause_vocabulary_matches_lean_model")
      "interrupt-and-cancel" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "ManagedExecState"
      "managed_exec::tests::rust_managed_exec_state_vocabulary_matches_lean_model")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "ToolFailureClass"
      "tool_call_lifecycle::tests::rust_failure_class_vocabulary_matches_lean_model")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "AwaitMode"
      "state_machine_conformance::lean_emits_await_mode_vocabulary")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "CancelPolicy"
      "state_machine_conformance::lean_emits_cancel_policy_vocabulary")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "ChildTerminal"
      "state_machine_conformance::lean_emits_child_terminal_vocabulary_and_projections")
      "background-tools" [Surface.agentFacing]
  ]

def stateMachineCoverage : List CoverageEntry :=
  [ tagged (consumerCoverage
      "state_machine"
      "Request"
      "lifecycle::tests::request_state_machine_contract_is_complete")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "Process"
      "runtime_status::tests::rust_process_state_transitions_match_lean_contract")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "Persistence.failClosed"
      boundaryStorageHookFailurePolicyId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "Persistence.failOpen"
      boundaryStorageHookFailurePolicyId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "StorageObservation.failClosed"
      boundaryStorageObservationDaemonVisibleId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "StorageObservation.failOpen"
      boundaryStorageObservationDaemonVisibleId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "RuntimeReconcile"
      "runtime_status::tests::runtime_reconcile_state_machine_contract_is_complete")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "PairingReconcile"
      "agent::reconcile::tests::pairing_reconcile_state_machine_contract_is_complete")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "SessionRecovery"
      "state_machine_conformance::generated_session_recovery_cases_drive_db_backed_reissue_contract")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "InferenceCall"
      "admission::tests::rust_inference_call_transition_table_matches_lean_contract")
      "inference-call" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "ToolCall"
      "tool_call_lifecycle::tests::tool_call_state_machine_contract_is_complete")
      "tool-call" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "ManagedExec"
      "managed_exec::tests::managed_exec_state_machine_contract_is_complete")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "state_machine"
      "AwaitMode"
      "state_machine_conformance::lean_emits_await_mode_vocabulary")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "state_machine"
      "CancelPolicy"
      "state_machine_conformance::lean_emits_cancel_policy_vocabulary")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "state_machine"
      "ChildTerminal"
      "state_machine_conformance::lean_emits_child_terminal_vocabulary_and_projections")
      "background-tools" [Surface.agentFacing]
  ]

def caseCoverage : List CoverageEntry :=
  [ tagged (consumerCoverage
      "lifecycle_transition_cases"
      "RequestTransitions"
      "state_machine_conformance::generated_request_transition_cases_cover_lifecycle_policy")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "lifecycle_transition_cases"
      "ProcessTransitions"
      "runtime_status::tests::generated_process_transition_cases_match_runtime_status_policy")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "trigger_cases"
      "TriggerDispatch"
      "trigger_engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "trigger_cases"
      "TriggerDispatch"
      "cli_config_task_run::config_task_run_matches_lean_manual_dispatch_contract")
      "triggers" [Surface.operatorCli]
  , tagged (consumerCoverage
      "trigger_cases"
      "TriggerDispatch"
      "defra_agent_desktop_tauri::bridge::snapshot::tests::runtime::task_recent_runs_view_consumes_generated_trigger_dispatch_lineage_contract_cases")
      "triggers" [Surface.operatorUi]
  , tagged (consumerCoverage
      "runtime_cases"
      "RuntimeReconcileCases"
      "runtime_status::tests::runtime_status_generation_updates_match_lean_runtime_reconcile_cases")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "apply_reconcile_cases"
      "ApplyReconcileCases"
      "config_import::lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary")
      "apply-reconcile" [Surface.operatorCli]
  , tagged (consumerCoverage
      "session_recovery_cases"
      "SessionRecoveryCases"
      "state_machine_conformance::generated_session_recovery_cases_drive_db_backed_reissue_contract")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "slot_cases"
      "InferenceCallSlotAccounting"
      "admission::tests::generated_inference_slot_accounting_cases_match_admission_reconstruction_logic")
      "inference-call" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "fleet_cases"
      "FleetSlotAccounting"
      boundaryFleetSlotAccountingDerivedViewId
      "admission::tests::generated_slot_accounting_fleet_cases_match_admission_runtime_boundary")
      "fleet-slot-accounting" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "fleet_cases"
      "FleetSlotAccounting"
      "cli_server::server_exposes_fleet_slot_snapshot_endpoint")
      "fleet-slot-accounting" [Surface.api]
  , tagged (boundaryCoverage
      "persistence_policy_cases"
      "PersistenceFailurePolicyCases"
      boundaryStorageHookFailurePolicyId
      "hook::tests::generated_persistence_failure_policy_cases_match_hook_decisions")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "storage_observation_cases"
      "StorageObservationRuntimeCases"
      boundaryStorageObservationDaemonVisibleId
      "hook::tests::generated_storage_observation_cases_match_hook_runtime_classification")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "backend_health_cases"
      "BackendHealthAdmissionCases"
      boundaryBackendHealthAdmissionFreshnessId
      "backend_registry::tests::generated_backend_health_admission_cases_match_registry_and_admission_policy")
      "backend-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "backend_health_cases"
      "BackendHealthAdmissionCases"
      "backend_registry::tests::display_state_matches_every_lean_backend_health_admission_case")
      "backend-health" [Surface.operatorUi]
  , tagged (consumerCoverage
      "native_filesystem_boundary_cases"
      "NativeFilesystemBoundaryCases"
      "toolset::tests::generated_native_filesystem_boundary_cases_match_preemptible_boundary_contract")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "managed_exec_cases"
      "ManagedExecLivenessCases"
      "state_machine_conformance::managed_exec_liveness_cases_pin_native_process_boundary")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "frontend_client_shell_cases"
      "FrontendClientShellCases"
      "apps/desktop-tauri/src/lib/chat-shell.test.ts::projectChatShell matches generated Lean ClientShell projection contracts")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "desktop_client_shell_cases"
      "DesktopClientShellCases"
      "defra_agent_desktop_tauri::bridge::snapshot::tests::session_state::session_snapshot_projection_consumes_generated_client_shell_contract_cases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "live_overlay_cases"
      "LiveOverlayCases"
      "live_overlay_conformance::live_overlay_cases_match_lean_table")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "request_lifecycle_operator_ui_cases"
      "RequestLifecycleOperatorUiCases"
      "defra_agent_desktop_tauri::bridge::snapshot::tests::session_state::session_snapshot_binds_request_lifecycle_operator_ui_cases")
      "request-lifecycle" [Surface.operatorUi]
  , tagged (consumerCoverage
      "tool_cases"
      "ToolExecutionPreflight"
      "state_machine_conformance::generated_tool_execution_cases_cover_preflight_and_retry_contracts")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "tool_cases"
      "ToolExecutionRetry"
      "mcp_pool::tests::tool_retry_disposition_contract_cases_match_mcp_pool_policy")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "command_policy_cases"
      "CommandPolicyValidation"
      "toolset::tests::generated_command_policy_cases_match_rust_validation")
      "command-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "command_policy_cases"
      "CommandPolicySandbox"
      "toolset::tests::generated_command_sandbox_cases_match_rust_selection")
      "command-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "command_policy_cases"
      "CommandPolicyEnv"
      "toolset::tests::generated_command_env_cases_match_rust_filtering")
      "command-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "queue_deadline_cases"
      "QueueDeadlineConformanceCases"
      "state_machine_conformance::generated_queue_deadline_cases_pin_r4a_contract_rows")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases"
      "state_machine_conformance::generated_recovery_sweep_cases_drive_startup_recovery_contract")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r6_background_cases"
      "R6BackgroundingCases"
      "state_machine_conformance::generated_r6_backgrounding_cases_pin_tool_backgrounding_contract")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "r5_cross_deployment_cases"
      "R5CrossDeploymentCases"
      "state_machine_conformance::generated_r5_cross_deployment_cases_drive_production_dispatch")
      "subagents-cross-deployment" [Surface.agentFacing]
  , tagged (consumerCoverage
      "r5_cross_deployment_cases"
      "R5CrossDeploymentCases"
      "http::r5_dispatch::tests::subagent_dispatch_endpoint_matches_agent_request_parent_walk")
      "subagents-cross-deployment" [Surface.api]
  , tagged (consumerCoverage
      "r5_cross_deployment_cases"
      "R5CrossDeploymentCases"
      "defra_agent_desktop_tauri::bridge::snapshot::tests::subagent_lineage::subagent_tree_view_consumes_generated_r5_cross_deployment_contract_cases")
      "subagents-cross-deployment" [Surface.operatorUi]
  , tagged (consumerCoverage
      "r6_background_theorem_witnesses"
      "BackgroundBudgetBoundedTheoremWitness"
      "state_machine_conformance::generated_r6_background_theorem_witnesses_drive_admission_budget_invariant")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r6_background_theorem_witnesses"
      "CascadeCancelsChildTheoremWitness"
      "state_machine_conformance::generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "r4c_background_work_cases"
      "R4cBackgroundWorkCases"
      "state_machine_conformance::generated_r4c_background_work_cases_pin_observable_shapes")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "r4c_background_work_cases"
      "R4cBackgroundWorkCases"
      "defra_agent_desktop_tauri::bridge::snapshot::operations_snapshot::tests::project_filters_to_background_await_mode_only")
      "background-tools" [Surface.operatorUi]
  , tagged (consumerCoverage
      "codex_shim_projection_cases"
      "CodexShimProjectionCases"
      "state_machine_conformance::generated_codex_shim_projection_cases_pin_adapter_mapping")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "transcript_cases"
      "TranscriptConformanceCases"
      "state_machine_conformance::generated_transcript_cases_drive_agent_message_ordering_contract")
      "transcript" [Surface.agentFacing]
  , tagged (consumerCoverage
      "identity_structural_cases"
      "IdentityStructuralCases"
      "identity_conformance::identity_structural_cases_match_lean_verdicts")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "identity_permission_cases"
      "IdentityPermissionCases"
      "identity_conformance::identity_permission_cases_pin_runtime_permission_contract_shape")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "identity_permission_cases"
      "IdentityPermissionCases"
      "http::identity_decide::tests::identity_decide_endpoint_matches_lean_permission_cases")
      "identity-permission" [Surface.api]
  , tagged (consumerCoverage
      "identity_contracts"
      "IdentityContracts"
      "identity_conformance::identity_respects_principal_contract_enforced_by_runtime_routing")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "streaming_response_cases"
      "ResponseTransitionCases"
      "state_machine_conformance::generated_streaming_response_cases_pin_lifecycle_contract")
      "streaming-response" [Surface.agentFacing]
  , tagged (consumerCoverage
      "streaming_response_interrupt_flow_cases"
      "ResponseInterruptFlowCases"
      "state_machine_conformance::generated_streaming_response_interrupt_flow_cases_drive_daemon_contract")
      "streaming-response" [Surface.agentFacing]
  , tagged (consumerCoverage
      "streaming_response_cases"
      "ResponseTransitionCases"
      "defra_agent_desktop_tauri::bridge::snapshot::tests::session_state::session_snapshot_streaming_response_overlay_consumes_generated_transition_cases")
      "streaming-response" [Surface.operatorUi]
  , tagged (consumerCoverage
      "compaction_reducer_cases"
      "CompactionReducerCases"
      "state_machine_conformance::generated_compaction_reducer_cases_pin_contract")
      "compaction" [Surface.agentFacing]
  , tagged (consumerCoverage
      "event_delivery_cases"
      "EventDeliveryTransitionCases"
      "state_machine_conformance::event_delivery_transition_cases_match_contract")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "event_delivery_cases"
      "EventDeliverySourceInstances"
      "state_machine_conformance::event_delivery_source_instances_match_runtime")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "event_delivery_cases"
      "EventDeliveryConvergenceTraces"
      "state_machine_conformance::event_delivery_convergence_traces_match_runtime_or_deviation")
      "event-delivery" [Surface.runtimeInternal]
  -- Closed by #253: the Rust consumer drives the full K=1 and K>=2 domain.
  , tagged (consumerCoverage
      "mcp_health_cases"
      "MCPHealthCases"
      "health_checker::tests::generated_mcp_health_cases_match_health_checker_transitions")
      "mcp-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "mcp_health_cases"
      "MCPHealthCases"
      "cli_mcp_probe::mcp_probe_json_reports_health_snapshot_for_registry_service")
      "mcp-health" [Surface.operatorCli]
  -- Closed by #278: the desktop bridge view consumes the same Lean transitions
  -- and asserts the K-model bookkeeping survives row -> view projection.
  , tagged (consumerCoverage
      "mcp_health_cases"
      "MCPHealthCases"
      "defra_agent_desktop_tauri::bridge::snapshot::tests::mcp_health::mcp_health_view_preserves_every_generated_lean_mcp_health_case_transition")
      "mcp-health" [Surface.operatorUi]
  , tagged (consumerCoverage
      "vocabulary"
      "CancelCause"
      "defra_agent_desktop_tauri::bridge::snapshot::tests::session_state::session_snapshot_derives_cancel_cause_for_interrupted_response_and_cancelled_tool_call")
      "interrupt-and-cancel" [Surface.operatorUi]
  , tagged (consumerCoverage
      "state_machine"
      "ToolCall"
      "defra_agent_desktop_tauri::bridge::tests::operations_cascade::preview_returns_four_classified_groups_and_a_signature")
      "interrupt-and-cancel" [Surface.operatorUi]
  , tagged (consumerCoverage
      "state_machine"
      "Request"
      "defra_agent_desktop_tauri::bridge::tests::operations_interrupt::interrupt_request_cascade_returns_accepted_when_signature_matches")
      "interrupt-and-cancel" [Surface.operatorUi]
  ]

def followUpHookCoverage : List CoverageEntry :=
  [ tagged (followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.foreground_blocks_parent_advance"
      "Subagent.BridgedState.foreground_blocks_parent_advance proves live foreground tools block parent progress/message advance; related aliases: Subagent.BridgedState.subagent_depth_bounded and Subagent.BridgedState.bridge_link_symmetric. Accepted Lean-only today because the invariant is a proof-layer bridge guard rather than an emitted runtime witness.")
      "background-tools" []
  , tagged (followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.bridged_child_completion_propagates"
      "Subagent.BridgedState.bridged_child_completion_propagates proves child completion projects to parent bridge-tool completion; related failure projection: Subagent.BridgedState.bridged_child_failure_projects. Accepted Lean-only today because R6Background emits data-shape cases and this theorem remains a formal trace projection.")
      "background-tools" []
  , tagged (followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.inv_depth"
      "Subagent.BridgedState.inv_depth proves bridged traces preserve max subagent depth; related link invariant: Subagent.BridgedState.inv_link. Accepted Lean-only today because these are structural trace invariants with no current Rust witness surface.")
      "background-tools" []
  , tagged (followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.bridgedUniqueCallIds_preserved"
      "Subagent.BridgedState.bridgedUniqueCallIds_preserved proves parent and child tool call ids remain unique across bridged traces. Accepted Lean-only today because the theorem lifts a structural uniqueness proof rather than an operational R6 witness.")
      "background-tools" []
  ]

def followUpHookIds : List String :=
  followUpHookCoverage.map (fun entry => entry.domain)

def followUpHooksJson : String :=
  jsonArray (followUpHookIds.map jsonString)

def coverageLedger : List CoverageEntry :=
  vocabularyCoverage ++ stateMachineCoverage ++ caseCoverage ++ followUpHookCoverage

structure FeatureMatrixCell where
  feature : String
  surface : Surface
  coverageStrength : String
  rowCount : Nat
  pendingFollowUps : Nat
  deferredNote : String
  deriving Repr

def featureSurfaceRequirementsJson : String :=
  jsonArray (featureSurfaceRequirements.map FeatureSurfaceRequirement.toJson)

def stringPresent (value : String) : Bool :=
  !(value == "")

def rowCoverageStrength (entry : CoverageEntry) : String :=
  let hasConsumer := stringPresent entry.consumer
  let hasBoundary := stringPresent entry.acceptedBoundary
  let hasFollowUp := stringPresent entry.acceptedFollowUp
  if hasConsumer && !hasFollowUp then
    "consumer"
  else if hasConsumer && hasFollowUp then
    "consumer_with_follow_up"
  else if !hasConsumer && hasBoundary then
    "boundary"
  else if !hasConsumer && !hasBoundary && hasFollowUp then
    "follow_up_only"
  else
    "missing"

def rowHasSurface (surface : Surface) (entry : CoverageEntry) : Bool :=
  entry.surfaces.any (fun candidate => candidate == surface)

def matchingFeatureSurfaceRows (feature : String) (surface : Surface) : List CoverageEntry :=
  coverageLedger.filter (fun entry =>
    (entry.feature == feature) && rowHasSurface surface entry)

def rowsHaveStrength (rows : List CoverageEntry) (strength : String) : Bool :=
  rows.any (fun entry => rowCoverageStrength entry == strength)

def strongestCoverageStrength (rows : List CoverageEntry) : String :=
  if rowsHaveStrength rows "consumer" then
    "consumer"
  else if rowsHaveStrength rows "consumer_with_follow_up" then
    "consumer_with_follow_up"
  else if rowsHaveStrength rows "boundary" then
    "boundary"
  else if rowsHaveStrength rows "follow_up_only" then
    "follow_up_only"
  else
    "missing"

def pendingFollowUpCount (rows : List CoverageEntry) : Nat :=
  (rows.filter (fun entry => stringPresent entry.acceptedFollowUp)).length

def requiredSurface (req : FeatureSurfaceRequirement) (surface : Surface) : Bool :=
  req.required.any (fun candidate => candidate == surface)

def deferredSurfaceNote (req : FeatureSurfaceRequirement) (surface : Surface) : Option String :=
  match req.deferred.find? (fun deferred => deferred.1 == surface) with
  | some deferred => some deferred.2
  | none => none

def featureMatrixCell? (req : FeatureSurfaceRequirement)
    (surface : Surface) : Option FeatureMatrixCell :=
  let rows := matchingFeatureSurfaceRows req.feature surface
  match rows with
  | [] =>
      match deferredSurfaceNote req surface with
      | some note =>
          some
            { feature := req.feature
            , surface := surface
            , coverageStrength := "deferred"
            , rowCount := 0
            , pendingFollowUps := 0
            , deferredNote := note
            }
      | none =>
          if requiredSurface req surface then
            some
              { feature := req.feature
              , surface := surface
              , coverageStrength := "missing"
              , rowCount := 0
              , pendingFollowUps := 0
              , deferredNote := ""
              }
          else
            none
  | _ :: _ =>
      some
        { feature := req.feature
        , surface := surface
        , coverageStrength := strongestCoverageStrength rows
        , rowCount := rows.length
        , pendingFollowUps := pendingFollowUpCount rows
        , deferredNote := ""
        }

def FeatureMatrixCell.toJson (cell : FeatureMatrixCell) : String :=
  "{"
    ++ "\"coverage_strength\":" ++ jsonString cell.coverageStrength ++ ","
    ++ "\"row_count\":" ++ toString cell.rowCount ++ ","
    ++ "\"pending_follow_ups\":" ++ toString cell.pendingFollowUps ++ ","
    ++ "\"deferred_note\":" ++ jsonString cell.deferredNote
    ++ "}"

def featureMatrixSurfaceCellJson? (req : FeatureSurfaceRequirement)
    (surface : Surface) : Option String :=
  match featureMatrixCell? req surface with
  | some cell => some (Surface.toJson surface ++ ":" ++ FeatureMatrixCell.toJson cell)
  | none => none

def featureMatrixFeatureJson (req : FeatureSurfaceRequirement) : String :=
  jsonString req.feature ++ ":"
    ++ "{"
    ++ String.intercalate ","
      (allSurfaces.filterMap (fun surface => featureMatrixSurfaceCellJson? req surface))
    ++ "}"

def featureMatrixJson : String :=
  "{"
    ++ String.intercalate "," (featureSurfaceRequirements.map featureMatrixFeatureJson)
    ++ "}"

def CoverageEntry.toJson (entry : CoverageEntry) : String :=
  "{"
    ++ "\"category\":" ++ jsonString entry.category ++ ","
    ++ "\"domain\":" ++ jsonString entry.domain ++ ","
    ++ "\"consumer\":" ++ jsonString entry.consumer ++ ","
    ++ "\"accepted_boundary\":" ++ jsonString entry.acceptedBoundary ++ ","
    ++ "\"accepted_follow_up\":" ++ jsonString entry.acceptedFollowUp ++ ","
    ++ "\"feature\":" ++ jsonString entry.feature ++ ","
    ++ "\"surfaces\":" ++ surfacesJson entry.surfaces
    ++ "}"

def coverageLedgerJson : String :=
  jsonArray (coverageLedger.map CoverageEntry.toJson)

end Conformance.Contracts
