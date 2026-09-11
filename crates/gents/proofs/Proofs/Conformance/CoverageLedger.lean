import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Boundaries

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

def consumerWithFollowUp
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
  , { feature := "request-execution-lease"
    , required := [Surface.agentFacing, Surface.runtimeInternal]
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
  , { feature := "completion-retry"
    , required := [Surface.agentFacing, Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "tool-call"
    , required := [Surface.agentFacing, Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "composed-invariants"
    , required := [Surface.runtimeInternal]
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
  , { feature := "session-hydration"
    , required := [Surface.runtimeInternal]
    , deferred := [(Surface.operatorUi, "client hydration/progress is #1143")]
    }
  , { feature := "authenticated-enrollment"
    , required := [Surface.runtimeInternal]
    , deferred :=
        [ (Surface.operatorCli, "operator approval wiring is completed later in #1293")
        , (Surface.operatorUi, "status-first enrollment UI is completed later in #1293")
        , (Surface.api, "status offer and enrollment status APIs are completed later in #1293")
        , (Surface.agentFacing, "enrollment authorizes transport; it is not agent-facing")
        ]
    }
  , { feature := "runtime-reconcile"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "graph-pipeline"
    , required := [Surface.runtimeInternal]
    , deferred :=
        [ (Surface.agentFacing, "evaluation-only custom tool; production configuration wiring follows graduation")
        , (Surface.operatorCli, "post-experiment graduation surface")
        , (Surface.operatorUi, "post-experiment graduation surface")
        , (Surface.api, "post-experiment graduation surface")
        ]
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
  , { feature := "descendant-graph"
    , required := [Surface.agentFacing, Surface.runtimeInternal, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "subagents-cross-principal"
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
    , required := [Surface.agentFacing, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "prompt-assembly"
    , required := [Surface.agentFacing, Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "rendered-capture"
    , required := [Surface.runtimeInternal, Surface.operatorCli, Surface.operatorUi]
    , deferred := []
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
  , { feature := "durable-goals"
    , required := [Surface.agentFacing, Surface.runtimeInternal, Surface.operatorCli]
    , deferred :=
        [ (Surface.operatorUi, "Bind goal projection to emitted cases; current card tests check rendering, not the continuation decision.")
        , (Surface.api, "Bind goal protocol operations to emitted decisions; current shim round-trip tests check persistence only.")
        ]
    }
  , { feature := "command-policy"
    , required := [Surface.agentFacing, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "tool-policy"
    , required := [Surface.agentFacing, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "self-config"
    , required := [Surface.agentFacing]
    , deferred :=
        [ (Surface.api, "MCP self-config surface deferred until MCP calls carry a DID (#654)")
        ]
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
    , required := [Surface.runtimeInternal, Surface.operatorCli, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "isolated-workspaces"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "mailbox"
    , required := allSurfaces
    , deferred := []
    }
  , { feature := "eth-submission"
    , required := [Surface.agentFacing, Surface.runtimeInternal]
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
      "gents_desktop_core::client::mutations::chat::request::tests::generated_session_recovery_cases_drive_desktop_retry_request")
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
      "CompletionRetryFailureClass"
      "conformance::completion_retry_lean_witness_cases_hold")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "vocabulary"
      "ToolRetryDisposition"
      "No production disposition enum exists; the deleted test-only mirror was not runtime vocabulary coverage. Retry behavior is observed through MCP operations below.")
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
      "conformance::tool_call::lean_emits_await_mode_and_cancel_policy_vocabularies")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "CancelPolicy"
      "conformance::tool_call::lean_emits_await_mode_and_cancel_policy_vocabularies")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "ChildTerminal"
      "conformance::tool_call::lean_emits_child_terminal_vocabulary")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "GoalStatus"
      "conformance::goals::rust_goal_status_vocabulary_and_machine_match_lean_contract")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "MailboxStatus"
      "conformance::mailbox::rust_mailbox_vocabularies_and_machine_match_lean_contract")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "vocabulary"
      "MailboxKind"
      "conformance::mailbox::rust_mailbox_vocabularies_and_machine_match_lean_contract")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "vocabulary"
      "MailboxHandling"
      "conformance::mailbox::rust_mailbox_vocabularies_and_machine_match_lean_contract")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "vocabulary"
      "MailboxSourceKind"
      "conformance::mailbox::rust_mailbox_vocabularies_and_machine_match_lean_contract")
      "mailbox" allSurfaces
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
      "runtime_status::tests::generated_process_transition_cases_match_runtime_status_policy")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "Persistence.failClosed"
      boundaryStorageHookFailurePolicyId
      "conformance::lean_executable_contracts_cover_initial_domains")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "Persistence.failOpen"
      boundaryStorageHookFailurePolicyId
      "conformance::lean_executable_contracts_cover_initial_domains")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "StorageObservation.failClosed"
      boundaryStorageObservationDaemonVisibleId
      "conformance::lean_executable_contracts_cover_initial_domains")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "StorageObservation.failOpen"
      boundaryStorageObservationDaemonVisibleId
      "conformance::lean_executable_contracts_cover_initial_domains")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "RuntimeReconcile"
      "runtime_status::tests::runtime_reconcile_state_machine_contract_is_complete")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "SessionRecovery"
      "gents_desktop_core::client::mutations::chat::request::tests::generated_session_recovery_cases_drive_desktop_retry_request")
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
      "EthSubmission"
      "eth::submit::tests::transition_table_matches_lean_contract")
      "eth-submission" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "Goal"
      "conformance::goals::rust_goal_status_vocabulary_and_machine_match_lean_contract")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "Mailbox"
      "conformance::mailbox::rust_mailbox_vocabularies_and_machine_match_lean_contract")
      "mailbox" [Surface.runtimeInternal]
  ]

def caseCoverage : List CoverageEntry :=
  [ tagged (consumerWithFollowUp
      "pairing_reconcile_cases"
      "PairingReconcileCases"
      "conformance::pairing_reconcile::generated_pairing_reconcile_cases_drive_production_projector"
      "The consumer exercises the production resource diff over emitted multi-resource snapshots. Connected flags are observations; actual transport dial/failure and applying operations to reach the post-state still require the transport reconciliation owner.")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "child_failure_projections"
      "ChildFailureProjections"
      "background_tools::tests::generated_child_failure_projections_match_bridge_owner")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "lifecycle_transition_cases"
      "RequestTransitions"
      "conformance::generated_request_transition_cases_cover_lifecycle_policy")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "provider_eof_cases"
      "ProviderEofCases"
      "lean_vocab_test::request_execution_lease_policy::generated_provider_eof_cases_fence_production_policy")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "request_execution_lease_cases"
      "RequestExecutionLeaseCases"
      "lean_vocab_test::request_execution_lease_policy::generated_request_execution_lease_cases_fence_production_policy"
      "Covers production begin/progress/finalize/revocation guards. Abstract claim freshness and recovery effects still require generated database consumer coverage.")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "request_execution_lease_trace_cases"
      "RequestExecutionLeaseTraceCases"
      "Lean-first #1341 race contract. Runtime recovery tests must consume these generated expiry/drop, stale-owner, and single terminal-effect traces when the lease is implemented.")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "lifecycle_transition_cases"
      "ProcessTransitions"
      "runtime_status::tests::generated_process_transition_cases_match_runtime_status_policy")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "trigger_cases"
      "TriggerDispatch"
      "trigger_engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases"
      "Dispatch decisions exercise the production engine. Existing materializer tests cover correlation scope, retry exclusion and live-execution supersession. Cross-principal isolation and expired-claim deadline/grace cases still need direct materializer observations. A booted schedule-kind error-writeback observation is also outstanding.")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "trigger_cases"
      "TriggerDispatch"
      "cli_config_task_run::config_task_run_matches_lean_manual_dispatch_contract")
      "triggers" [Surface.operatorCli]
  , tagged (consumerCoverage
      "trigger_cases"
      "TriggerDispatch"
      "gents_desktop_bridge::snapshot::tests::runtime::task_recent_runs_view_consumes_generated_trigger_dispatch_lineage_contract_cases")
      "triggers" [Surface.operatorUi]
  , tagged (consumerCoverage
      "goal_decision_cases"
      "GoalDecisionCases"
      "conformance::goals::generated_goal_decision_cases_fence_runtime_controller")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_transition_cases"
      "GoalTransitionCases"
      "conformance::goals::generated_goal_transition_cases_fence_runtime_state_machine")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_create_cases"
      "GoalCreateCases"
      "conformance::goals::generated_goal_create_cases_fence_authority_and_idempotency")
      "durable-goals" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_capability_resolution_cases"
      "GoalCapabilityResolutionCases"
      "conformance::generated_goal_capability_resolution_matches_rust_decoder")
      "durable-goals" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "task_goal_publication_cases"
      "TaskGoalPublicationCases"
      "conformance::goals::generated_task_goal_cases_fence_declaration_and_identity"
      "The consumer checks the production declaration validator and fire identity. Atomic task/goal/request publication needs the shared publication owner; fixture-only booleans are not an implementation check.")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "task_goal_recovery_cases"
      "TaskGoalRecoveryCases"
      "conformance::goals::generated_task_goal_recovery_cases_fence_request_witness")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_submission_cases"
      "GoalSubmissionCases"
      "conformance::goals::generated_goal_submission_cases_fence_atomic_visibility")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_continuation_materialization_cases"
      "GoalContinuationMaterializationCases"
      "conformance::goals::generated_goal_continuation_materialization_cases_fence_restart_idempotency")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "session_hydration_cases"
      "SessionHydrationDecisionCases"
      "conformance::session_hydration::generated_session_hydration_cases_match_decision_core")
      "session-hydration" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "session_hydration_progress_cases"
      "SessionHydrationProgressCases"
      "conformance::session_hydration::generated_session_hydration_progress_cases_match_observe")
      "session-hydration" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "session_hydration_durable_cases"
      "SessionHydrationDurableCases"
      "conformance::session_hydration::generated_session_hydration_durable_cases_match_storage_projection")
      "session-hydration" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "enrollment_cases"
      "EnrollmentCases"
      "conformance::enrollment::generated_enrollment_cases_match_production_transition_core")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "enrollment_encoding_cases"
      "EnrollmentEncodingCases"
      "conformance::enrollment::generated_enrollment_encoding_vectors_match_wire_codec")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "enrollment_digest_cases"
      "EnrollmentDigestCases"
      "conformance::enrollment::generated_enrollment_digest_vectors_match_wire_codec")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "event_group_cases"
      "EventGroupCases"
      "trigger_engine::tests::event_group_eligibility_matches_lean_generated_contract_cases"
      "Exercises the production count/timeout eligibility owner for trigger and callback cases. Typed consumer/config/owner identity, durable marker suppression and atomic materialization require the migrated event-group owner; the old trigger-only spy cannot establish these guarantees.")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "enrollment_durable_projection_cases"
      "EnrollmentDurableProjectionCases"
      "conformance::enrollment::generated_enrollment_durable_cases_drive_current_projection")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "pending_user_turn_cases"
      "PendingUserTurnCases"
      "conformance::live_overlay::pending_user_turn_cases_match_lean_table")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "aggregate_token_budget_cases"
      "AggregateTokenBudgetCases"
      "agent::loop_stream::tests::generated_aggregate_token_budget_cases_drive_the_owned_loop_ledger"
      "Exercises charged-usage summation and the owned loop budget ledger. Database selection of restart rows and missing-usage rejection still need completion-owner observations.")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "request_progress_cases"
      "RequestProgressCases"
      "packages/gents-desktop-chat/src/chat-shell.test.ts::requestProgressPresentation matches every generated Lean request lifecycle projection")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "agent_request_admission_cases"
      "AgentRequestAdmissionCases"
      "conformance::enrollment::generated_agent_request_admission_cases_match_shared_projector")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_decision_cases"
      "GoalDecisionCases"
      "goal_continuation_live::durable_goal_continues_with_real_inference_until_model_completes")
      "durable-goals" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "runtime_cases"
      "RuntimeReconcileCases"
      "runtime_status::tests::runtime_status_generation_updates_match_lean_runtime_reconcile_cases"
      "The status consumer observes publication/router-generation writeback. Generated request acceptance, lifetime tracking and generation retirement need router/reconciler owner consumers; fixture-only arithmetic is not implementation coverage.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "startup_readiness_cases"
      "StartupReadinessCases"
      "conformance::generated_startup_readiness_cases_pin_bounded_barrier_release")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "apply_reconcile_cases"
      "ApplyReconcileCases"
      "Regenerate the atomic-publication witnesses and bind them to the common config transaction owner. Rows invoke ApplyReconcile.publish directly; the old per-write Rust adapter does not implement this contract.")
      "apply-reconcile" [Surface.operatorCli]
  , tagged (consumerCoverage
      "tool_policy_cases"
      "ToolPolicyCases"
      "conformance::generated_tool_policy_cases_match_lean_composition")
      "tool-policy" [Surface.operatorUi, Surface.agentFacing]
  , tagged (consumerCoverage
      "lsp_action_cases"
      "LspActionCases"
      "conformance::generated_lsp_action_cases_match_rust_authorization")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "self_config_field_tables"
      "SelfConfigFieldTables"
      "conformance::self_config_field_tables_match_lean_contract")
      "self-config" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "self_config_cases"
      "SelfConfigCases"
      "conformance::generated_self_config_cases_fence_patch_merge"
      "Covers production patch admissibility and accepted merges. Nested Tools no-lockout, reference validation and unchanged stored state after rejection require the shared configuration transaction owner.")
      "self-config" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "session_recovery_cases"
      "SessionRecoveryCases"
      "gents_desktop_core::client::mutations::chat::request::tests::generated_session_recovery_cases_drive_desktop_retry_request"
      "Exercises desktop retry eligibility and successor lineage. Admission lease release remains an owner refinement: terminal-failed is the current adapter assumption, and emitted admission witnesses are not independently observed. Exact-row retryFromRows cases require the migrated query/transaction owner.")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "slot_cases"
      "InferenceCallSlotAccounting"
      "conformance::generated_inference_slot_accounting_cases_drive_db_backed_reconstruction")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "logical_output_obligation_cases"
      "LogicalOutputObligationCases"
      "agent::output_obligation::logical_tests::generated_logical_output_obligation_cases_drive_signed_requests_and_durable_writes")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "completion_retry_cases"
      "completionRetry"
      "conformance::completion_retry_lean_witness_cases_hold"
      "Retry/failure/output-obligation cases drive production decisions. reissue_with_open_effects_illegal and rendered_never_two require owned-loop effect-closure and rendered-response traces, not assertions on expected fixture fields.")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "fleet_cases"
      "FleetSlotAccounting"
      boundaryFleetSlotAccountingDerivedViewId
      "admission::tests::generated_slot_accounting_fleet_cases_match_admission_runtime_boundary")
      "fleet-slot-accounting" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "fleet_cases"
      "FleetSlotAccounting"
      "conformance::generated_slot_accounting_cases_pin_inference_and_fleet_contracts")
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
      "backend_health_cases"
      "BackendHealthTransitionCases"
      "backend_health::tests::generated_backend_health_cases_match_prober_transitions")
      "backend-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "inference_registry_cases"
      "InferenceRegistryCases"
      "admission::registry::contract_tests::generated_inference_registry_cases_drive_real_permits")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "backend_health_cases"
      "BackendHealthTransitionCases"
      "http::prometheus::tests::backend_probe_status_metric_reflects_measured_health")
      "backend-health" [Surface.operatorCli]
  , tagged (followUpCoverage
      "native_filesystem_boundary_cases"
      "NativeFilesystemBoundaryCases"
      "Replay generated glob/grep/list_files cases through the existing managed filesystem boundary. toolset::tests::native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue preserves real GlobTool preemption/queue observations; grep/list_files routing is not established by the removed fixture-only test.")
      "tool-call" [Surface.agentFacing]
  , tagged (followUpCoverage
      "managed_exec_cases"
      "ManagedExecLivenessCases"
      "Drive generated exit/deadline/cancel cases through existing managed_exec process tests. Fixture kill flags do not observe OS termination.")
      "managed-exec" [Surface.agentFacing]
  , tagged (followUpCoverage
      "managed_exec_cases"
      "ManagedExecToolBoundaryCases"
      "Drive each generated native-tool route through its actual managed process boundary. Names and processTree flags are not evidence of runtime routing.")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "pairing_reconcile_cases"
      "PairingReconcileShutdownBoundaryCases"
      "conformance::pairing_reconcile_shutdown_boundary_preempts_in_flight_sweep")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "pairing_reconcile_cases"
      "PairingReconcileSweepRetryBoundaryCases"
      "conformance::pairing_reconcile_top_level_sweep_failure_is_nonterminal_and_retried")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "pairing_reconcile_cases"
      "PairingReconcileSweepSchedulingCases"
      "conformance::pairing_reconcile_sweep_does_not_head_of_line_block_ready_peer")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "frontend_client_shell_cases"
      "FrontendClientShellCases"
      "packages/gents-desktop-chat/src/chat-shell.test.ts::projectChatShell matches generated Lean ClientShell projection contracts")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "desktop_client_shell_cases"
      "DesktopClientShellCases"
      "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_projection_consumes_generated_client_shell_contract_cases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_behavior_readiness_cases"
      "ClientBehaviorReadinessCases"
      "conformance::client_runtime::generated_behavior_readiness_cases_drive_the_production_projector")
      "runtime-reconcile" [Surface.operatorUi]
  , tagged (consumerCoverage
      "live_overlay_cases"
      "LiveOverlayCases"
      "gents_desktop_bridge::snapshot::tests::session_timeline::session_snapshot_consumes_generated_live_overlay_cases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "request_lifecycle_operator_ui_cases"
      "RequestLifecycleOperatorUiCases"
      "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_binds_request_lifecycle_operator_ui_cases")
      "request-lifecycle" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "tool_cases"
      "ToolExecutionPreflight"
      "meta_tools::call::tests::generated_tool_preflight_cases_match_health_and_schema_gates"
      "Generated inputs exercise the production health and schema gates. Full call_tool dispatch and typed health-denial propagation still need generated owner observations.")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "tool_cases"
      "ToolExecutionRetry"
      "mcp_pool::tests::list_tools_transport_failure_retries_generated_safe_read_case"
      "Drives the real safe-read retry after a transport failure. The remaining generated failure/idempotency matrix needs production owner consumers.")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "tool_cases"
      "ToolExecutionRetry"
      "mcp_pool::tests::call_tool_transport_failure_obeys_generated_no_retry_cases_without_idempotency_metadata"
      "Drives a real failed call without idempotency metadata and observes no retry or eviction. Explicit idempotency metadata and native-command cases are not exercised by this path.")
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
      "command_policy_cases"
      "CommandPolicyOperatorUi"
      "gents_desktop_bridge::snapshot::tests::session_timeline::structured_command_policy_denial_projects_to_rendered_tool")
      "command-policy" [Surface.operatorUi]
  , tagged (consumerCoverage
      "queue_deadline_cases"
      "QueueDeadlineConformanceCases"
      "conformance::generated_queue_deadline_cases_pin_r4a_contract_rows")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  -- Inference cases include periodic cadence: startup may defer a live execution
  -- lease, so the same existing call owner must run after later request repair.
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases"
      "conformance::generated_recovery_sweep_cases_drive_startup_recovery_contract")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "graph_failure_attribution_traces"
      "GraphFailureAttributionTraces"
      "graph_pipeline::run::attribution_contract_tests::generated_graph_failure_attribution_traces_drive_real_transactions"
      "Covers durable CAS outcomes and emitted positive failure decisions through the real interrupt owner. Isolated no-failure/cancellation suppression remains unobserved; a false failure-interrupt decision does not forbid cancellation-driven interrupts.")
      "graph-pipeline" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "graph_workspace_lineage_cases"
      "GraphWorkspaceLineageCases"
      "graph_pipeline::run::workspace_lineage_contract_tests::generated_graph_workspace_cases_drive_installed_plan_and_signed_receipts")
      "graph-pipeline" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "artifact_mode_meet_cases"
      "ArtifactModeMeetCases"
      "toolset::tests::generated_artifact_mode_meet_cases_drive_command_effect_intersection")
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  -- Admission uses real signed requests/bindings and managed-launch policy.
  -- Unsupported-platform coverage supplies the unavailable-host observation to
  -- the production selector; kernel enforcement remains an external boundary.
  , tagged (boundaryCoverage
      "artifact_admission_cases"
      "ArtifactAdmissionCases"
      boundaryCommandPolicyHostExecutionAssumptionsId
      "workspace::overlay::overlay_tests::generated_artifact_admission_cases_drive_live_binding_and_launch_policy")
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  -- One consumer covers all five cases: real foreground/background launches
  -- carrying the same grant, plus persistent LSP denial before pool dispatch.
  -- The spawned-task witness does not claim durable background-bridge coverage.
  , tagged (boundaryCoverage
      "artifact_spawn_cases"
      "ArtifactSpawnCases"
      boundaryCommandPolicyHostExecutionAssumptionsId
      "toolset::tests::generated_artifact_spawn_cases_drive_live_foreground_and_background_launches")
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "operator_base_freeze_cases"
      "OperatorBaseFreezeCases"
      "workspace::tests::operator_base_freeze::generated_operator_base_freeze_cases_drive_real_git_executor")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "workspace_path_capability_cases"
      "WorkspacePathCapabilityCases"
      "workspace::tests::generated_workspace_path_capability_cases_drive_real_git_executor")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "invalid_tool_progress_cases"
      "InvalidToolProgressCases"
      "agent::loop_stream::tests::generated_invalid_tool_progress_cases_drive_owned_loop")
      "completion-retry" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "workspace_path_alias_cases"
      "WorkspacePathAliasCases"
      "workspace::tests::path_alias_contract::generated_workspace_path_alias_cases_drive_real_git_delta")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "graph_logical_invocation_cases"
      "GraphLogicalInvocationCases"
      "graph_pipeline::logical_invocation_contract_tests::generated_graph_logical_invocations_drive_persisted_run_projection"
      "Fifteen representable cases replay through signed production owners. The defensive pinned-root-with-parent row is outside admission: root event/schedule authority excludes Goal/local-control parent authority, and ancestry excludes its entry. Reconcile that abstract input domain before claiming runtime projection coverage; do not broaden authority to manufacture the row.")
      "graph-pipeline" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "graph_invocation_publication_cases"
      "GraphInvocationPublicationCases"
      "graph_pipeline::run::publication_contract_tests::generated_graph_invocation_publication_traces_drive_real_transactions"
      "Covers durable publication, failure/cancellation fences and child counts. Failure-driven interrupts still need observations through the graph execution owner; recomputing may_interrupt_for_failure from stored flags does not exercise that decision.")
      "graph-pipeline" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_claimed_publication_cases"
      "GoalClaimedPublicationCases"
      "goal::claimed_publication::contract_tests::generated_goal_claimed_publication_cases_drive_real_transactions")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_request_head_cases"
      "GoalRequestHeadCases"
      "goal::request_head::tests::generated_goal_request_head_cases_drive_signed_row_selector")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_operator_resume_cases"
      "GoalOperatorResumeCases"
      "goal::operator_resume::contract_tests::generated_goal_operator_resume_cases_drive_real_transactions")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_operator_resume_cases"
      "GoalOperatorResumeCases"
      "cli_goal::goal_resume_request_reuses_signed_predecessor_and_returns_same_child")
      "durable-goals" [Surface.operatorCli]
  , tagged (consumerCoverage
      "goal_config_reactivation_cases"
      "GoalConfigReactivationCases"
      "goal::operator_resume::contract_tests::generated_goal_config_reactivation_cases_drive_transactional_setter")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "graph_pipeline_validation_cases"
      "GraphPipelineValidationCases"
      "conformance::graph_pipeline::generated_validation_cases_fence_whole_graph_compilation_gate")
      "graph-pipeline" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "graph_pipeline_revision_gate_cases"
      "GraphPipelineRevisionGateCases"
      "conformance::graph_pipeline::generated_revision_gate_cases_fence_publication_and_start_readiness")
      "graph-pipeline" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "graph_pipeline_run_terminal_cases"
      "GraphPipelineRunTerminalCases"
      "conformance::graph_pipeline::generated_run_terminal_cases_fence_completion_cas")
      "graph-pipeline" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "restart_disposition_cases"
      "RestartDispositionCases"
      "conformance::generated_restart_disposition_cases_drive_recover_all")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r6_background_cases"
      "R6BackgroundingCases"
      "conformance::generated_r6_backgrounding_cases_drive_tool_backgrounding_contract")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "r5_cross_principal_cases"
      "R5CrossPrincipalCases"
      "conformance::generated_r5_cross_principal_cases_drive_production_dispatch")
      "subagents-cross-principal" [Surface.agentFacing]
  , tagged (consumerCoverage
      "r5_cross_principal_cases"
      "R5CrossPrincipalCases"
      "http::r5_dispatch::tests::subagent_dispatch_endpoint_matches_agent_request_parent_walk")
      "subagents-cross-principal" [Surface.api]
  , tagged (consumerCoverage
      "r5_cross_principal_cases"
      "R5CrossPrincipalCases"
      "gents_desktop_bridge::snapshot::tests::subagent_lineage::subagent_tree_view_consumes_generated_r5_cross_principal_contract_cases")
      "subagents-cross-principal" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "composed_invariant_witnesses"
      "ComposedInvariantWitnesses"
      "conformance::generated_composed_invariant_witnesses_drive_tool_lifecycle_conformance"
      "Covers persisted tool recovery/cancellation outcomes for four representative deadline/interrupt inputs. Full composed request, admission and clock traces still need their runtime owners; fixture path assertions are not replay.")
      "composed-invariants" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "cancel_propagation_cases"
      "CancelPropagationCases"
      "conformance::cancel_propagation_cases_drive_production_interrupt")
      "interrupt-and-cancel" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r6_background_theorem_witnesses"
      "BackgroundBudgetBoundedTheoremWitness"
      "conformance::generated_r6_background_theorem_witnesses_drive_admission_budget_invariant")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r6_background_theorem_witnesses"
      "CascadeCancelsChildTheoremWitness"
      "conformance::generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "subagent_delegation_graph_cases"
      "SubagentDelegationGraphCases"
      "conformance::delegation_depth_matches_runtime_limit"
      "This consumer compares the runtime depth limit only. Generated path acyclicity, boundedness and cascade witnesses need actual delegation/control traces; asserting their expected flags is not implementation coverage.")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "descendant_graph_cases"
      "DescendantGraphCases"
      "descendant_graph::tests::generated_descendant_graph_cases_fence_visibility_and_control")
      "descendant-graph" [Surface.agentFacing, Surface.runtimeInternal, Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "r4c_background_work_cases"
      "R4cBackgroundWorkCases"
      "conformance::unmaterialized_child_status_matches_runtime_vocabulary"
      "This consumer compares runtime status vocabulary only. Existing subagent e2e tests exercise visibility and steering; generated lineage rejection, cursor, append and interrupt observations need those owner consumers. Fixture-only assertions did not establish them.")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "r4c_background_work_cases"
      "R4cBackgroundWorkCases"
      "gents_desktop_bridge::snapshot::operations_snapshot::tests::project_filters_to_background_await_mode_only")
      "background-tools" [Surface.operatorUi]
  , tagged (consumerCoverage
      "r4c_background_work_cases"
      "R4cBackgroundWorkCases"
      "conformance::generated_read_tool_output_witness_drives_hook_dispatch")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "tool_output_paging_cases"
      "ToolOutputPagingCases"
      "background_tools::tests::generated_tool_output_paging_cases_match_slice_function")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "bridge_step_cases"
      "BridgeStepCases"
      "conformance::generated_bridge_step_cases_drive_bridge_lifecycle")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "codex_shim_projection_cases"
      "CodexShimProjectionCases"
      "conformance::generated_codex_shim_projection_cases_pin_adapter_mapping"
      "Exercises the canonical persisted-attempt projector, followed by test-local phase/status mapping. Drive the gents-cli Codex shim mapping and local-interrupt override before claiming end-to-end shim projection coverage.")
      "codex-shim" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_subagent_tool_cases"
      "CodexShimSubagentToolCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "codex_shim_subagent_status_cases"
      "CodexShimSubagentStatusCases"
      "conformance::generated_codex_shim_projection_cases_pin_adapter_mapping"
      "Exercises the canonical persisted-attempt projector, followed by test-local phase/status mapping. Drive the gents-cli Codex shim mapping and local-interrupt override before claiming end-to-end shim projection coverage.")
      "codex-shim" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_subagent_visibility_cases"
      "CodexShimSubagentVisibilityCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_subagent_metadata_cases"
      "CodexShimSubagentMetadataCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_subagent_listing_cases"
      "CodexShimSubagentListingCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_subagent_thread_shape_cases"
      "CodexShimSubagentThreadShapeCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "codex_shim_reasoning_projection_cases"
      "CodexShimReasoningProjectionCases"
      "commands::codex_shim::turn_projection::tests::generated_reasoning_projection_cases_drive_turn_projection_notifications")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "codex_shim_thread_status_cases"
      "CodexShimThreadStatusCases"
      "conformance::generated_codex_shim_projection_cases_pin_adapter_mapping"
      "Exercises the canonical persisted-attempt projector, followed by test-local phase/status mapping. Drive the gents-cli Codex shim mapping and local-interrupt override before claiming end-to-end shim projection coverage.")
      "codex-shim" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_behavior_selection_cases"
      "CodexShimBehaviorSelectionCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_tool_metadata_cases"
      "CodexShimToolMetadataCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_context_usage_cases"
      "CodexShimContextUsageCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_compaction_projection_cases"
      "CodexShimCompactionProjectionCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "codex_shim_turn_lifecycle_cases"
      "CodexShimTurnLifecycleCases"
      "The conformance suite's copied mappings and fixture-field assertions do not exercise the gents-cli Codex shim owner. Route these generated inputs through its production projection before claiming adapter coverage.")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "codex_shim_binding_cases"
      "CodexShimBindingCases"
      "conformance::generated_codex_shim_binding_cases_pin_runnable_gated_binding")
      "codex-shim" [Surface.api, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "transcript_cases"
      "TranscriptConformanceCases"
      "conformance::generated_transcript_cases_drive_agent_message_ordering_contract")
      "transcript" [Surface.agentFacing]
  , tagged (consumerCoverage
      "transcript_cases"
      "TranscriptConformanceCases"
      "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_transcript_rendering_consumes_generated_transcript_cases")
      "transcript" [Surface.operatorUi]
  , tagged (followUpCoverage
      "identity_structural_cases"
      "IdentityStructuralCases"
      "Exercise canonical owner-scoped registry validation; the deleted test-local well-formedness predicate was not implementation conformance.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "identity_permission_cases"
      "IdentityPermissionCases"
      "conformance::identity::resolved_identity_permission_cases_drive_defra_acp"
      "Checks native Defra ACP after explicit principal resolution. Unknown-owner and same-owner-collision selector rejection require the canonical registry owner; no test-local ID map remains.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "identity_permission_cases"
      "IdentityPermissionCases"
      "http::identity_decide::tests::identity_decide_endpoint_matches_resolved_lean_permission_cases")
      "identity-permission" [Surface.api]
  , tagged (followUpCoverage
      "identity_contracts"
      "IdentityContracts"
      "Route through the canonical principal-scoped registry and exercise rejection before permission checks. The removed synthetic global-ID map did not exercise runtime routing.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "streaming_response_cases"
      "ResponseTransitionCases"
      "conformance::generated_streaming_response_cases_pin_lifecycle_contract"
      "Observes response status, live tail, token count, materialization marker and request lifecycle. Full durable-reasoning transfer and atomic response/request commit require the completion/materialization owner; this adapter only marks an externally supplied materialization sequence.")
      "streaming-response" [Surface.agentFacing]
  , tagged (consumerCoverage
      "streaming_response_interrupt_flow_cases"
      "ResponseInterruptFlowCases"
      "conformance::generated_streaming_response_interrupt_flow_cases_drive_daemon_contract")
      "streaming-response" [Surface.agentFacing]
  , tagged (consumerCoverage
      "streaming_response_cases"
      "ResponseTransitionCases"
      "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_streaming_response_overlay_consumes_generated_transition_cases")
      "streaming-response" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "compaction_reducer_cases"
      "CompactionReducerCases"
      "conformance::generated_compaction_reducer_cases_pin_contract"
      "Strip/provider-view cases exercise actual payload reduction and reapplication. Summarize cases exercise the production gate and splitter only; full checkpoint execution and same-operation idempotence need the compaction owner consumer.")
      "compaction" [Surface.agentFacing]
  , tagged (consumerCoverage
      "compaction_cursor_cases"
      "CompactionCursorCases"
      "conformance::generated_compaction_reducer_cases_pin_contract")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_cases"
      "PromptAssemblySanitizeCases"
      "conformance::prompt_assembly::generated_sanitize_cases_drive_the_production_sanitizer")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_cases"
      "PromptAssemblyLayerCases"
      "agent::loop_stream::tests::generated_layer_cases_pin_the_assembled_request_order")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "prompt_assembly_cases"
      "PromptAssemblyRepairCases"
      "agent::loop_stream::tests::generated_repair_cases_drive_tool_argument_repair")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_cases"
      "PromptAssemblyBudgetCases"
      "agent::loop_stream::tests::generated_budget_cases_drive_dynamic_output_compaction_trigger")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "prompt_assembly_cases"
      "PromptAssemblyTurnBudgetCases"
      "agent::loop_stream::tests::generated_turn_budget_cases_drive_every_completion_dispatch")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "prompt_assembly_cases"
      "PromptAssemblyRetentionCases"
      "agent::loop_stream::tests::generated_retention_cases_drive_production_compaction_target")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_cases"
      "PromptAssemblyClaudeMapCases"
      "conformance::prompt_assembly::generated_claude_map_cases_drive_the_messages_parser")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_cases"
      "PromptAssemblyClaudeBodyCases"
      "conformance::prompt_assembly::generated_claude_body_cases_drive_the_body_builder")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_cases"
      "PromptAssemblyClaudeStreamCases"
      "conformance::prompt_assembly::generated_claude_stream_cases_drive_the_messages_parser")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "rendered_capture_cases"
      "RenderedCaptureCases"
      "agent::loop_stream::tests::generated_rendered_capture_cases_fence_persist_before_send"
      "Drives provider-send ordering with scripted sink outcomes. Durable fresh/idempotent/conflicting bindings are observed separately by the real sink consumer; no test-side store model remains.")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "RenderedCaptureCases"
      "agent::loop_stream::tests::generated_rendered_capture_cases_hold_against_the_real_defra_sink")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "durable_reduction_cases"
      "DurableReductionCases"
      "provider_context_reduction::durable_reduction_conformance::generated_durable_reduction_cases_pin_storage_and_capture_citations"
      "Checks durable create/load/conflict and capture citations. The exported send_permitted fence is not exercised: validate actual provider dispatch against durable reduction facts through the owned completion loop.")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rolling_compaction_cases"
      "RollingCompactionCases"
      "compaction::tests::generated_rolling_cases_drive_the_production_commit_precondition")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "reduction_engine_cases"
      "ReductionEngineCases"
      "compaction::tests::generated_reduction_engine_cases_drive_shared_decision_outcome")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "budget_rehydration_cases"
      "BudgetRehydrationCases"
      "completion_factory::tests::rehydrates_aggregate_budget_from_durable_inference_calls"
      "Drives absent, zero and positive pinned limits with inference/compaction rows through the real physical-request query and ledger factory. Full process restart and InferenceCall write ownership remain separate boundaries.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "RenderedCaptureKeyCases"
      "conformance::rendered_capture::generated_rendered_capture_key_cases_pin_the_capture_key_tuple")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "CaptureScopeCases"
      "conformance::rendered_capture::generated_capture_scope_cases_pin_the_shared_parser_and_order")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "CaptureOrderCases"
      "conformance::rendered_capture::generated_capture_scope_cases_pin_the_shared_parser_and_order")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "RenderedCaptureCases"
      "cli_trace_export::trace_capture_fetches_metadata_with_field_commit_cid")
      "rendered-capture" [Surface.operatorCli]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "RenderedCaptureCases"
      "apps/gents-desktop/tests/request-trace.test.tsx::request trace panel renders the reconstructed event stream")
      "rendered-capture" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "event_delivery_cases"
      "EventDeliveryTransitionCases"
      "conformance::event_delivery_transition_cases_match_contract"
      "Observes five Watcher rescan/next-request cases, including real cooldown seeding. Eight substrate bookkeeping rows remain unobserved; subscription loss/delivery, queue multiset state, and empty-rescan silence need owner observations rather than a copied World.")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "event_delivery_cases"
      "EventDeliveryTransitionCases"
      "trigger_engine::tests::event_source::generated_sibling_delivery_case_preserves_pending_correlation"
      "Observes the sibling trigger readiness/correlation case. Other transition rows use separate consumers or retain the explicit substrate follow-up.")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "event_delivery_cases"
      "EventDeliverySourceInstances"
      "conformance::event_delivery_source_instances_match_runtime")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "event_delivery_cases"
      "EventDeliveryConvergenceTraces"
      "conformance::event_delivery_convergence_traces_match_runtime_or_deviation"
      "Observes persisted documents recovered by real rescans and emitted request identities for all three sources. Does not assert full final World state, monotone-once silence on later rescans, or subscription delivery/multiset semantics.")
      "event-delivery" [Surface.runtimeInternal]
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
  , tagged (consumerCoverage
      "mcp_health_cases"
      "MCPHealthCases"
      "gents_desktop_bridge::snapshot::tests::mcp_health::mcp_health_view_preserves_every_generated_lean_mcp_health_case_transition")
      "mcp-health" [Surface.operatorUi]
  , tagged (consumerCoverage
      "vocabulary"
      "CancelCause"
      "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_derives_cancel_cause_for_interrupted_response_and_cancelled_tool_call")
      "interrupt-and-cancel" [Surface.operatorUi]
  , tagged (consumerCoverage
      "state_machine"
      "ToolCall"
      "gents_desktop_bridge::tests::operations_cascade::preview_returns_four_classified_groups_and_a_signature")
      "interrupt-and-cancel" [Surface.operatorUi]
  , tagged (consumerCoverage
      "state_machine"
      "Request"
      "gents_desktop_bridge::tests::operations_interrupt::interrupt_request_cascade_returns_accepted_when_signature_matches")
      "interrupt-and-cancel" [Surface.operatorUi]
  , tagged (followUpCoverage
      "workspace_cases"
      "WorkspaceCases"
      "Replay these cases through the production Workspace lifecycle owner after its canonical principal fields migrate; the deleted test-local state table was not implementation coverage.")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "workspace_binding_cases"
      "WorkspaceBindingCases"
      "Replay these cases through the production Workspace binding admission owner after principal-field migration; do not restore the deleted test-local binding predicate.")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "callback_cases"
      "CallbackCases"
      "conformance::callback_lifecycle::generated_callback_journals_match_runtime_owner"
      "Generated journal-prefix observations exercise the actual journal owner. Invocation-state legality, denied execution and result-emission ordering need the callback executor consumer; the removed Rust predicate copy did not establish them.")
      "isolated-workspaces" [Surface.runtimeInternal]

  , tagged (consumerWithFollowUp "runtime_cases" "RuntimeReconcileCases"
      "agent::runtime::tests::behavior_resolution::explicit_behavior_resolution_matches_lean_binding_cases"
      "Exercises explicit request/session behavior binding. Readiness, atomic admission and generation lifetime remain router-owner obligations.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp "request_input_cases" "RequestInputCases"
      "conformance::request_input::lean_request_inputs_decode_without_losing_explicit_issuance_facts"
      "Canonical input serde only. Signed canonical_input_fields/bytes, context whitelist, title materialization and verified goal receipt checks need the migrated admission/signing owners.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "session_document_cases" "SessionDocumentCases"
      "Canonical session DB/lifecycle and fork tests specify target behavior; the generated interned-ID selection/projection/retry/fork rows still need real-owner adapters, including exact physical references and transactional freshness. No fixture-local state machine may substitute.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "background_wake_row_cases" "BackgroundWakeRowCases"
      "Consume exact physical-parent and cross-requester authoritative rows through background publication after its owner migrates. Existing DB wake tests do not establish every generated row verdict.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "configuration_scope_cases" "ConfigurationScopeCases"
      "Resolve same-label documents through the real owner-qualified context/inference registry; do not rebuild Lean lookup in tests.")
      "apply-reconcile" [Surface.runtimeInternal]
  , tagged (followUpCoverage "discovery_scope_cases" "DiscoveryScopeCases"
      "Exercise backend-owner and credential-scoped catalog selection in the production discovery owner, including shared/OAuth and foreign-owner rejection.")
      "apply-reconcile" [Surface.runtimeInternal]
  , tagged (followUpCoverage "event_group_clock_cases" "EventGroupClockCases"
      "Drive the existing durable group clock owner after typed trigger/callback identity migration; preserve first-seen and quiescence across restart.")
      "triggers" [Surface.runtimeInternal]
  , tagged (followUpCoverage "event_group_capture_cases" "EventGroupCaptureCases"
      "Drive captured ordered input and typed origin through the callback capture owner; quiesced groups must reject capture.")
      "triggers" [Surface.runtimeInternal]
  , tagged (followUpCoverage "callback_transition_cases" "CallbackTransitionCases"
      "Drive the callback executor's claim/run/succeed/fail/denial operations and observe exact input/origin/journal/emission; journal-prefix-only checks cannot establish lifecycle coverage.")
      "triggers" [Surface.runtimeInternal]

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
      "Subagent.BridgedState.subagent_depth_bounded"
      "Subagent.BridgedState.subagent_depth_bounded proves bridged traces preserve max subagent depth; related link invariant: Subagent.BridgedState.bridge_link_symmetric. The arbitrary graph-level closure is emitted through subagent_delegation_graph_cases; this hook remains for the paired bridge trace invariant.")
      "background-tools" []
  , tagged (followUpCoverage
      "follow_up_hook"
      "Subagent.BridgedState.bridgedUniqueCallIds_preserved"
      "Subagent.BridgedState.bridgedUniqueCallIds_preserved proves parent and child tool call ids remain unique across bridged traces. Accepted Lean-only today because the theorem lifts a structural uniqueness proof rather than an operational R6 witness.")
      "background-tools" []
  , tagged (boundaryCoverage
      "follow_up_hook"
      "StreamingResponse.Transition.streamIdleTimeout.deadlinePrecondition"
      boundaryStreamingResponseIdleTimeoutDeadlineId)
      "streaming-response" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "follow_up_hook"
      "PromptAssembly.providerInput.sanitizeLoadedHistory"
      boundaryPromptAssemblyProviderInputSanitizationId)
      "prompt-assembly" [Surface.agentFacing]
  , tagged (boundaryCoverage
      "follow_up_hook"
      "Compaction.safeToReduce.sessionScopeResolver"
      boundaryCompactionSafeToReduceSessionScopeId)
      "compaction" [Surface.agentFacing]
  , tagged (boundaryCoverage
      "follow_up_hook"
      "Compaction.providerViewAppend.uniqueCallIdsChecked"
      boundaryCompactionUniqueCallIdsCheckedId)
      "compaction" [Surface.agentFacing]
  , tagged (followUpCoverage
      "follow_up_hook"
      "PromptAssembly.Template.assembled_preamble_literal"
      "The existing slot assembler preserves resolved context instructions literally; task_binding_preserves_context confines invocation substitutions to the task slot. Task render_determined proves dependency on declared task variables. The next layers must fence this model through the real provider-input serializer; slot content preservation alone is not a wire-format proof.")
      "prompt-assembly" []
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
  if hasBoundary then
    "boundary"
  else if hasConsumer && !hasFollowUp then
    "consumer"
  else if hasConsumer && hasFollowUp then
    "consumer_with_follow_up"
  else if hasFollowUp then
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
