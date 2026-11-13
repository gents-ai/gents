import Proofs.Conformance.ContractTypes

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
  /-- Whether a native test consumes the family. The consumer scan in
  `gents-lean-vocab` finds that test from the family's snapshot key; a row
  never names it. -/
  consumed : Bool
  acceptedBoundary : String
  acceptedFollowUp : String
  feature : String := ""
  surfaces : List Surface := []
  deriving Repr

def consumerCoverage
    (category domain : String) : CoverageEntry :=
  { category := category
  , domain := domain
  , consumed := true
  , acceptedBoundary := ""
  , acceptedFollowUp := ""
  }

def boundaryCoverage
    (category domain acceptedBoundary : String) : CoverageEntry :=
  { category := category
  , domain := domain
  , consumed := false
  , acceptedBoundary := acceptedBoundary
  , acceptedFollowUp := ""
  }

def followUpCoverage
    (category domain acceptedFollowUp : String) : CoverageEntry :=
  { category := category
  , domain := domain
  , consumed := false
  , acceptedBoundary := ""
  , acceptedFollowUp := acceptedFollowUp
  }

def consumerWithFollowUp
    (category domain acceptedFollowUp : String) : CoverageEntry :=
  { category := category
  , domain := domain
  , consumed := true
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
  , { feature := "account-rotation"
    , required := [Surface.runtimeInternal]
    , deferred := [(Surface.operatorUi,
        "The rotation pool and each account's limit state surface with the account settings in the CLI and desktop.")]
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
  , { feature := "task-hooks"
    , required := [Surface.runtimeInternal]
    , deferred :=
        [ (Surface.operatorUi,
           "Desktop already edits and displays Task.hooks; there is no runtime observation to project until the host executor records attempts.") ]
    }
  , { feature := "pairing-reconcile"
    , required := [Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "file-inventory"
    , required := [Surface.runtimeInternal]
    , deferred := []
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
  , { feature := "interrupt-and-cancel"
    , required := [Surface.agentFacing, Surface.operatorUi]
    , deferred :=
        [ (Surface.operatorCli, "#266")
        ]
    }
  , { feature := "session-control"
    , required := [Surface.agentFacing, Surface.runtimeInternal]
    , deferred := []
    }
  , { feature := "lifecycle-hooks"
    , required := [Surface.runtimeInternal]
    , deferred := []
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
  , { feature := "eval"
    , required := [Surface.operatorCli]
    , deferred := [(Surface.operatorUi, "#1515")]
    }
  , { feature := "optimization"
    , required := [Surface.operatorCli]
    , deferred := [(Surface.operatorUi, "#1455")]
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
  , { feature := "canonical-output"
    , required := [Surface.runtimeInternal, Surface.agentFacing, Surface.operatorUi]
    , deferred := []
    }
  , { feature := "client-shell"
    , required := [Surface.operatorUi]
    , deferred := []
    }
  , { feature := "durable-goals"
    , required := [Surface.agentFacing, Surface.runtimeInternal, Surface.operatorCli]
    , deferred :=
        [ (Surface.operatorUi, "Bind goal projection to emitted cases; current card tests check rendering, not the continuation decision.")
        , (Surface.api, "Bind goal protocol operations to emitted decisions.")
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
        , (Surface.operatorCli, "SelfConfig.agentAdmission governs CLI agent create/clone/edit/disable/default (#3656); bind once the operations-layer CLI adapter decides through it over ACP grants (#3654)")
        , (Surface.operatorUi, "SelfConfig.agentAdmission governs desktop agent save/patch/set-default (#3656); bind once the desktop adapter decides through it over ACP grants (#3654)")
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
  , { feature := "plugin-jobs"
    , required := [Surface.agentFacing, Surface.runtimeInternal, Surface.operatorCli, Surface.operatorUi]
    , deferred := []
    }
  ]

def vocabularyCoverage : List CoverageEntry :=
  [ tagged (consumerCoverage
      "vocabulary"
      "RequestState")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "execution_origin_vocabulary")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "process_state_vocabulary")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "vocabulary"
      "PersistenceState"
      "The database schema has no per-token PersistenceState document; committed/uncommitted is the model abstraction.")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "vocabulary"
      "PersistenceFailurePolicy"
      "The database has no per-token persistence document; fail-closed and fail-open hook decisions are replayed through persistence_failure_policy_cases.")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "reconcile_phase_vocabulary")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "vocabulary"
      "StorageObservation"
      "Local mutation acknowledgements, stale reads and visible rows are observations of DefraDB, not proofs of its internal commit algorithm.")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "SessionRecoveryLatestRequestState")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "InferenceCallState")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "InferenceCallTerminalReason")
      "inference-call" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "vocabulary"
      "CompletionRetryFailureClass"
      "The immutable-publication retry vocabulary has changed; the native owned loop must migrate before this is production vocabulary coverage.")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "vocabulary"
      "ToolRetryDisposition"
      "No production disposition enum exists; the deleted test-only mirror was not runtime vocabulary coverage. Retry behavior is observed through MCP operations below.")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_execution_tool_call_state_vocabulary")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_execution_cancel_cause_vocabulary")
      "interrupt-and-cancel" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "ManagedExecState")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "managed_exec_stop_outcome_vocabulary")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "managed_exec_cancel_reply_vocabulary")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "process_stop_cases"
      "ProcessStopCases"
      "Each generated verdict is checked against the native mapping, and the reachable observations (a recorded running group, the same group after the owner's stop, a reused start identity) are produced by real test-owned process groups. A group that survives SIGKILL cannot be constructed on a test host, so stillRunning is checked only through the mapping.")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_execution_failure_class_vocabulary")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_execution_await_mode_vocabulary")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "vocabulary"
      "GoalStatus")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "vocabulary"
      "MailboxStatus")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "vocabulary"
      "MailboxKind")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "vocabulary"
      "MailboxHandling")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "vocabulary"
      "MailboxSourceKind")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "vocabulary"
      "EvalOutcomeKind")
      "eval" [Surface.operatorCli]
  , tagged (consumerCoverage
      "vocabulary"
      "EvalProviderReason")
      "eval" [Surface.operatorCli]
  , tagged (consumerCoverage
      "vocabulary"
      "EvalEvidenceClass")
      "eval" [Surface.operatorCli]
  ]

def stateMachineCoverage : List CoverageEntry :=
  [ tagged (consumerCoverage
      "state_machine"
      "Request")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "Process")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "Persistence.failClosed"
      "The database has no per-token persistence document; fail-closed and fail-open hook decisions are replayed through persistence_failure_policy_cases.")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "Persistence.failOpen"
      "The database has no per-token persistence document; fail-closed and fail-open hook decisions are replayed through persistence_failure_policy_cases.")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "StorageObservation.failClosed"
      "Local mutation acknowledgements, stale reads and visible rows are observations of DefraDB, not proofs of its internal commit algorithm.")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "StorageObservation.failOpen"
      "Local mutation acknowledgements, stale reads and visible rows are observations of DefraDB, not proofs of its internal commit algorithm.")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "RuntimeReconcile")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "SessionRecovery")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "InferenceCall")
      "inference-call" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "ToolCall")
      "tool-call" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "ManagedExec")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "state_machine"
      "Goal")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "state_machine"
      "Mailbox")
      "mailbox" [Surface.runtimeInternal]
  ]

def caseCoverage : List CoverageEntry :=
  [ tagged (followUpCoverage
      "identity_client_agent_recovery_cases" "IdentityClientAgentRecoveryCases"
      "The snapshot emits and the decoder accepts these cases; no native test reads them.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "network_protocol_resource")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "identity_secret_publication_cases" "IdentitySecretPublicationCases"
      "The snapshot emits and the decoder accepts these cases; no native test reads them.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "remote_agent_delivery_recovery_cases" "RemoteAgentDeliveryRecoveryCases"
      "The snapshot emits and the decoder accepts these cases; no native test reads them.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "remote_agent_delivery_response_cases" "RemoteAgentDeliveryResponseCases"
      "The snapshot emits and the decoder accepts these cases; no native test reads them.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "scope_collection_rules" "ScopeCollectionRules"
      "The snapshot emits and the decoder accepts these cases; no native test reads them.")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "lifecycle_hook_owed_delivery_cases"
      "LifecycleHookOwedDeliveryCases"
      "The production record (hooks::owed::Record, owe_runner, derive_halts, prunable) is driven by every case. Its publication through the store's post-commit runtime write, the redrive's and the halt derivation's observation of core rows as the owning Agent, and the deletion of pruned rows are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "cross_actor_authorization_cases" "CrossActorAuthorization"
      "Native transaction-bound requester document and node authorization is unavailable; cross-actor execution remains blocked. Bind these cases to native transaction and claim owners when supported.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "goal_link_cases" "GoalLinks"
      "A detached relationship operation already in flight is not fenced by a concurrent revocation.")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "identity_parent_publication_cases" "ParentPublication"
      "Callback repair assumes trusted authored Agent metadata; forged or missing authenticated creator traces need an ownership-bound native consumer.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "identity_callback_parent_actor_cases" "CallbackParentActor")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "goal_client_delivery_cases" "GoalClientDeliveryCases")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage
      "root_admission_cases"
      "RootAdmissionCases")
      "apply-reconcile" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "pairing_reconcile_cases"
      "PairingReconcileCases"
      "The consumer exercises the production resource diff over emitted multi-resource snapshots. Connected flags are observations; actual transport dial/failure and applying operations to reach the post-state still require the transport reconciliation owner.")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "request_transition_cases"
      "RequestTransitions")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "request_execution_lease_contracts_provider_eof")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "request_execution_lease_contracts_observed_publication")
      "request-execution-lease" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "request_execution_lease_contracts_observed_recovery")
      "request-execution-lease" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "request_execution_lease_contracts_observed_recheck")
      "request-execution-lease" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "request_execution_lease_contracts_observed_world_unreached"
      "model-production-gap: no production owner computes the whole lease world after an observed transaction commits; finalization, revocation, drop and competing-renewal interleavings have no served scenario.")
      "request-execution-lease" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "account_rotation_contracts_selection")
      "account-rotation" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "account_rotation_contracts_attempts")
      "account-rotation" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "account_rotation_contracts_observation")
      "account-rotation" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "request_execution_lease_contracts_admission")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "request_execution_lease_contracts_renewal")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "request_execution_lease_contracts_relinquishment")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "request_execution_lease_contracts_step_world_unreached"
      "model-production-gap: no production owner computes the whole lease world; claim, finalization, recovery, revocation, observing-replica and clock steps have no native replay (#1571).")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "request_execution_lease_trace_cases"
      "RequestExecutionLeaseTraceCases"
      "#1571 generated bounded-renewal, expiry/drop, stale-owner and terminal-effect traces require native execution, including independent timer polling and suspend/resume. No scheduler fairness or wall-clock continuity is proved.")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "process_transition_cases"
      "ProcessTransitions")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "trigger_delivery_admission_cases"
      "Triggers.Durable")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_session_resolution")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_fire_budget_admission")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "trigger_delivery_queue_cases"
      "Triggers.Queue")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_delivery_options")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_observed_claim")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "trigger_delivery_cursor_cases"
      "EventDelivery.Durable")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "trigger_delivery_callback_cursor_cases"
      "EventDelivery.Durable")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "trigger_delivery_outcome_cases"
      "Triggers.Durable.Outcomes")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "trigger_dispatch_cases"
      "TriggerDispatch"
      "Dispatch decisions exercise the production engine. Existing materializer tests cover correlation scope, retry exclusion and live-execution supersession. Cross-node isolation and expired-claim deadline/grace cases still need direct materializer observations. A booted schedule-kind error-writeback observation is also outstanding.")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "trigger_dispatch_cases"
      "TriggerDispatch")
      "triggers" [Surface.operatorCli]
  , tagged (consumerCoverage
      "trigger_dispatch_cases"
      "TriggerDispatch")
      "triggers" [Surface.operatorUi]
  , tagged (consumerCoverage
      "goal_decision_cases"
      "GoalDecisionCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_readiness_gate_cases"
      "GoalReadinessGateCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_claimed_readiness_cases"
      "GoalClaimedReadinessCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_transition_cases"
      "GoalTransitionCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_create_cases"
      "GoalCreateCases")
      "durable-goals" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_capability_resolution_cases"
      "GoalCapabilityResolutionCases")
      "durable-goals" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "task_goal_publication_cases"
      "TaskGoalPublicationCases"
      "The consumer checks the production declaration validator and fire identity. Atomic task/goal/request publication needs the shared publication owner; fixture-only booleans are not an implementation check.")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "task_goal_recovery_cases"
      "TaskGoalRecoveryCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_submission_cases"
      "GoalSubmissionCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_continuation_materialization_cases"
      "GoalContinuationMaterializationCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_closure_cases"
      "FileInventoryClosureCases"
      "The native consumer checks closure construction against modeled rows, but supplies access and denial observations as fixture input. It does not verify live ACP decisions.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "enrollment_cases"
      "EnrollmentCases")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "enrollment_encoding_cases"
      "EnrollmentEncodingCases")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "enrollment_digest_cases"
      "EnrollmentDigestCases")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "event_group_cases"
      "EventGroupCases"
      "Exercises the production count/timeout eligibility owner for trigger and callback cases. Typed consumer/config/owner identity, durable marker suppression and atomic materialization require the migrated event-group owner; the old trigger-only spy cannot establish these guarantees.")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "enrollment_durable_projection_cases"
      "EnrollmentDurableProjectionCases")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "enrollment_pending_request_cases"
      "EnrollmentPendingRequests")
      "authenticated-enrollment" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_sync_health_cases"
      "ClientSyncHealthCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "contracts_pending_user_turn")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_queued_steering_trace_pending_terminal"
      "Binds the two true-pending interruptBeforeClaim and admissionReject scripts: signed raw content/input survive and no canonical output rows appear. It does not bind claimed or owned terminal paths, provider-send denial, capture conflict, exact replay, or pending-turn UI visibility.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_queued_steering_trace_owned_terminal"
      "Native binding covers the two modeled owned prepublication failure/interruption terminal-owner suffixes after a production-signed steering enqueue: claim/begin, physical interrupt latch where modeled, real NoMessage terminalization, retained signed content/input, and zero canonical output/active request. It does not inject a daemon context-preparation error or hook cancellation, execute provider dispatch, inspect the pending-turn UI projection, or verify fresh admission at claim. Those upstream and projection premises remain unbound.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_queued_steering_trace_publication"
      "Native binding covers only the publication-before-provider-stream-entry script: it sends the modeled prepared prompt and observes the durable authored header before the provider stream opens. It does not execute generated steering queue ingress, inspect the UI admissionVisible projection or serialized capture body, or prove native lease authorization. Claimed pre-start failure, prepublication send denial, conflicting capture, and exact replay scripts remain model-derived/decoder-checked only.")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (boundaryCoverage
      "family"
      "contracts_queued_steering_trace_trace_unreached"
      "model-production-gap: no native owner exposes the whole steering trace observation (admission visibility, capture binding, queue head and prepared candidate); claimed pre-start failure, prepublication send denial, conflicting capture and exact replay scripts have no native replay.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "pack_finality_cases"
      "PackFinalityCases"
      "Binds callback rejection, lost receipt and a later failed retry to the shared transaction owner.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_pack_installation_hosted_evidence"
      "Binds client signer, exact create and update graphs, and relationship scope to an embedded transaction. Full plan validation, deletion and native denial remain to be exercised.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "pack_wire_lock_cases"
      "PackWireLockCases")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_pack_installation_hosted_schema"
      "Binds scope signatures to the model; native Node authorization is checked by the runtime admission owner.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_pack_installation_hosted_home"
      "Binds actual home record write and failure to modeled success; native commit and cancellation ordering are exercised by the transaction owner.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_ownership")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "Proofs.Conformance.PackSourceSelection"
      "PackSourceSelectionCases")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "Proofs.Conformance.PackInstallVariables"
      "PackInstallVariableCases")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "Proofs.Conformance.PackInstallUpgrade"
      "PackInstallUpgradeCases")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "agent_steer_disposition_cases"
      "AgentSteerDispositionCases"
      "Binds refusal of incompatible contexts; signed cross-requester and workspace integration remain to be exercised.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "agent_steer_target_cases"
      "AgentSteerTargetCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_steering_boundary_selection"
      "Binds ordered boundary selection; held-tool delivery and exact replay need native flow coverage.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_steering_boundary_publication")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "agent_steer_reachability_cases"
      "AgentSteerReachabilityCases"
      "Binds admission against the ordered boundary owner, including compatible earlier steers.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_agent_delivery"
      "Binds delivery resolution; transaction admission and owned-loop delivery need native flow coverage.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "interrupt_delivery_cases"
      "InterruptDeliveryCases"
      "Binds the physical target selected in the message publication transaction; cross-node replication remains a separate boundary.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "agent_folded_binding_cases"
      "AgentFoldedBindingCases"
      "Binds requester and physical-head outcome checks; cross-node replication races remain to be exercised.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "agent_message_outcome_cases"
      "AgentMessageOutcomeCases"
      "Binds outcome selection; physical-head binding and notification replay need native flow coverage.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "queued_steering_guard_cases"
      "QueuedSteeringGuardCases"
      "Wrong-head claim, incoherent supplied-owner and interrupted publication guards are checked by the Lean owner and generated decoder/conformance structural tests only. The exact physical interrupt latch, like native interrupt_request_by_doc_id, permits terminal rows and preserves an existing stamp; lifecycle terminalization is separate. Bind each guard to the native queue/owned-execution rejection path before claiming native conformance.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "aggregate_token_budget_cases"
      "AggregateTokenBudgetCases"
      "Exercises charged-usage summation and the owned loop budget ledger. Database selection of restart rows and missing-usage rejection still need completion-owner observations.")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "task_hook_admission_cases"
      "TaskHookAdmissionCases")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "task_hook_run_cases"
      "TaskHookRunCases"
      "The consumer binds the emitted phase vocabulary to the production TaskHookPhase encoding and replays admission over every trace; the executor consumer below drives the traces.")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "task_hook_run_cases"
      "TaskHookRunCases"
      "Every generated trace runs through the production orchestration and managed host execution with real commands: each scripted result is produced by a real process (an exit status, a nonexistent executable, a command outliving its model-resolved timeout, or a held command cancelled through the interrupt or shutdown source its phase uses). The processes' own invocation log, with the owned work's entry, is compared against the emitted invocationTrace, excluding only occurrences that could not launch; attempts, outcomes and the terminal request state are compared too. The daemon placement of the before gate at claim and the after phases ahead of terminalization is fenced by daemon tests, not these traces.")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "task_hook_recovery_cases"
      "TaskHookRecoveryCases")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "request_progress_cases"
      "RequestProgressCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "agent_request_admission_cases"
      "AgentRequestAdmissionCases")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "causal_hop_contracts_step_admission")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "causal_hop_contracts_session_target")
      "request-lifecycle" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "causal_hop_contracts_session_list")
      "request-lifecycle" [Surface.agentFacing]
  , tagged (followUpCoverage
      "title_request_admission_cases"
      "TitleRequestAdmissionCases"
      "Model-derived signed title-purpose admission and pending-disposition inputs cover exact runtime-local-control parent-only provenance, forbidden request controls, unavailable observation retry, authoritative denial, and unchanged Pending on admit. Bind the generated rows to native signing, watcher admission, pending rejection and crash rediscovery owners before claiming native coverage.")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "title_request_purpose_wire_cases"
      "TitleRequestPurposeWireCases"
      "Model-derived missing, unknown and explicit purpose wire inputs require a strict native decoder and signed-field selection. No native consumer is bound; absent purpose must never default to normal.")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_decision_cases"
      "GoalDecisionCases")
      "durable-goals" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "runtime_reconcile_cases"
      "RuntimeReconcileCases"
      "The status consumer observes publication/router-generation writeback. Generated request acceptance, lifetime tracking and generation retirement need router/reconciler owner consumers; fixture-only arithmetic is not implementation coverage.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_claude_platform_outcome")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_catalog_refresh_scenarios")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_terminal_selection_observed")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_terminal_selection_hydration")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_identity_reference")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "pack_eval_subject_cases"
      "PackEvalSubjectCases")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_identity_plan")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_reference_cases"
      "PluginReferenceCases")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "snapshot_demotion_cases"
      "SnapshotDemotionCases")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "startup_readiness_cases"
      "StartupReadinessCases")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "readiness_publication_cases"
      "ReadinessPublicationCases"
      "Pending the node readiness publisher rename (L3). The native consumer drives semantic state changes and idle time through a controlled writer. Durable DefraDB publication and host/process failure behavior are not exercised by this writer fixture.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "apply_reconcile_contract_cases_publish_cases"
      "Regenerate the atomic-publication witnesses and bind them to the common config transaction owner. Rows invoke ApplyReconcile.publish directly; the old per-write Rust adapter does not implement this contract.")
      "apply-reconcile" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "apply_reconcile_contract_cases_publish_if_cases")
      "apply-reconcile" [Surface.operatorCli]
  , tagged (consumerCoverage
      "eval_outcome_cases"
      "EvalOutcomeCases")
      "eval" [Surface.operatorCli]
  , tagged (consumerCoverage
      "optimization_cases"
      "OptimizationCases")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage
      "optimization_cases"
      "OptimizationCases")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage
      "optimization_cases"
      "OptimizationCases")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage
      "optimization_cases"
      "OptimizationCases")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage
      "optimization_cases"
      "OptimizationCases")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage
      "tool_policy_cases"
      "ToolPolicyCases")
      "tool-policy" [Surface.operatorUi, Surface.agentFacing]
  , tagged (consumerCoverage
      "application_write_cases"
      "ApplicationWriteCases")
      "tool-policy" [Surface.agentFacing, Surface.operatorCli]
  , tagged (consumerCoverage
      "field_read_cases"
      "FieldReadCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "write_input_cases"
      "WriteInputCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "invocation_correlation_cases"
      "InvocationCorrelationCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_lsp_action_authorization")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "self_config_field_tables"
      "SelfConfigFieldTables")
      "self-config" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "self_config_cases"
      "SelfConfigCases"
      "Covers production patch admissibility and accepted merges, replays every Tools row through the always-on operator-grant guard (guard_tools_keep_grants) and every Context and Agent row, clone rows included, through reselection_keeps_grants, and replays guarded rows through the production Tools/Agent no-lockout guards, the Backend auth fence and the Profile account choice fence. Reference validation and unchanged stored state after rejection require the shared configuration transaction owner.")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "self_config_scope_cases"
      "SelfConfigScopeCases")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "agent_decision_cases"
      "AgentDecisionCases")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "agent_materialization_cases"
      "AgentMaterializationCases")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "sibling_tools_cases"
      "SiblingToolsCases")
      "self-config" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "session_recovery_cases"
      "SessionRecoveryCases"
      "Exercises desktop retry eligibility and successor lineage. Admission lease release remains an owner refinement: terminal-failed is the current adapter assumption, and emitted admission witnesses are not independently observed. Exact-row retryFromRows cases require the migrated query/transaction owner.")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "inference_slot_accounting_cases"
      "InferenceCallSlotAccounting")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "mailbox_notification_cases"
      "MailboxNotificationCases")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "mailbox_reply_cases"
      "MailboxReplyCases"
      "Decision cases cover reply matching and terminal-state eligibility. Signed-row loading, atomic request/item claim, and concurrent dismissal require transaction-owner integration tests.")
      "mailbox" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "mailbox_question_cases"
      "MailboxQuestionCases")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "mailbox_handoff_cases"
      "MailboxHandoffCases"
      "Only the fresh-linked positive case drives the stamped mailbox write, signed producer terminal owner, and linked reply admission in native storage. The other eleven generated handoff cases remain model-only; this consumer does not exercise AgentToolCall result publication, the owned completion loop, or atomic crash recovery across the separate mailbox and request transactions.")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "count_carrier_cases"
      "CountCarrierCases")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "logical_output_obligation_contracts_gate")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "completion_retry_cases"
      "completionRetry"
      "Retry policy and canonical-execution composition are executable Lean witnesses. The native owned loop must still implement retraction-before-retry and immutable acceptance before this is consumer coverage.")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "fleet_slot_accounting_cases"
      "FleetSlotAccounting"
      "The database persists InferenceCall rows, not a FleetState aggregate; the scheduler reconstruction assumes those rows can be observed.")
      "fleet-slot-accounting" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "fleet_slot_accounting_cases"
      "FleetSlotAccounting")
      "fleet-slot-accounting" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "fleet_slot_accounting_cases"
      "FleetSlotAccounting")
      "fleet-slot-accounting" [Surface.api]
  , tagged (consumerCoverage
      "persistence_failure_policy_cases"
      "PersistenceFailurePolicyCases")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "storage_observation_runtime_cases"
      "StorageObservationRuntimeCases"
      "Local mutation acknowledgements, stale reads and visible rows are observations of DefraDB, not proofs of its internal commit algorithm.")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "backend_health_cases"
      "BackendHealthAdmissionCases"
      "A provider models-endpoint probe does not establish completion health, remote-node reachability or fleet-wide document freshness.")
      "backend-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "backend_health_cases"
      "BackendHealthAdmissionCases")
      "backend-health" [Surface.operatorUi]
  , tagged (consumerCoverage
      "backend_health_cases"
      "BackendHealthTransitionCases")
      "backend-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "backend_probe_schedule_cases"
      "BackendProbeScheduleCases")
      "backend-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "inference_registry_admission")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "title_usage_cases"
      "TitleUsageCases"
      "The catalog consumer binds parent-inclusive spending, missing and zero usage, late reports, duplicate-call accounting and goal charges that include reviews while excluding titles. Historical receipt authentication and foreign-parent filtering remain separate native admission boundaries.")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "backend_health_cases"
      "BackendHealthTransitionCases")
      "backend-health" [Surface.operatorCli]
  , tagged (followUpCoverage
      "native_filesystem_boundary_cases"
      "NativeFilesystemBoundaryCases"
      "Replay generated glob/grep/list_files cases through the existing managed filesystem boundary. toolset::tests::native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue preserves real GlobTool preemption/queue observations; grep/list_files routing is not established by the removed fixture-only test.")
      "tool-call" [Surface.agentFacing]
  , tagged (followUpCoverage
      "managed_exec_liveness_cases"
      "ManagedExecLivenessCases"
      "Drive generated exit/deadline/cancel cases through existing managed_exec process tests. Fixture kill flags do not observe OS termination.")
      "managed-exec" [Surface.agentFacing]
  , tagged (followUpCoverage
      "managed_exec_tool_boundary_cases"
      "ManagedExecToolBoundaryCases"
      "Drive each generated native-tool route through its actual managed process boundary. Names and processTree flags are not evidence of runtime routing.")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "pairing_reconcile_cases"
      "PairingReconcileShutdownBoundaryCases")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "pairing_reconcile_cases"
      "PairingReconcileSweepRetryBoundaryCases")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "pairing_reconcile_cases"
      "PairingReconcileSweepSchedulingCases")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "client_installation_traces")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_installation_compatibility")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "runtime_service_replacement_traces")
      "runtime-reconcile" [Surface.runtimeInternal, Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "update_handoff_transitions")
      "runtime-reconcile" [Surface.runtimeInternal, Surface.operatorUi]
  , tagged (consumerCoverage
      "inference_observation_cases"
      "InferenceObservationCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "inference_visibility_cases"
      "InferenceVisibilityCases")
      "client-shell" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "client_optimistic_send_cases"
      "ClientOptimisticSendCases"
      "The desktop chat-send replay was removed when desktop sends moved onto the shared request handler; the replay must be rebuilt against that handler.")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "client_telemetry_cases"
      "ClientTelemetryCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_settings_cases"
      "ClientSettingsCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_data_publication_cases"
      "ClientDataNoticeDeliveryCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_request_patch_publication_cases"
      "ClientRequestPatchPublicationCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_session_delivery_direct_read")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "storage_failure_cases"
      "StorageFailureCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_request_closure_cases"
      "ClientRequestClosureCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_request_cache_identity_cases"
      "ClientRequestCacheIdentityCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "frontend_client_shell_cases"
      "FrontendClientShellCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "desktop_client_shell_cases"
      "DesktopClientShellCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_session_turn_cases"
      "ClientSessionTurnCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_session_queue_order_cases"
      "ClientSessionQueueOrderCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "client_agent_readiness_cases"
      "ClientAgentReadinessCases"
      "Pending: L2 renames the consumer test and L3 the production projector it drives.")
      "runtime-reconcile" [Surface.operatorUi]
  , tagged (consumerCoverage
      "live_overlay_cases"
      "LiveOverlayCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "client_live_delta_cases"
      "ClientLiveDeltaCases")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "request_lifecycle_operator_ui_cases"
      "RequestLifecycleOperatorUiCases")
      "request-lifecycle" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "tool_preflight_cases"
      "ToolExecutionPreflight"
      "Generated inputs exercise the production health and schema gates. Full call_tool dispatch and typed health-denial propagation still need generated owner observations.")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "tool_retry_cases"
      "ToolExecutionRetry"
      "Drives the real safe-read retry after a transport failure. The remaining generated failure/idempotency matrix needs production owner consumers.")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "tool_retry_cases"
      "ToolExecutionRetry"
      "Drives a real failed call without idempotency metadata and observes no retry or eviction. Explicit idempotency metadata and native-command cases are not exercised by this path.")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "compaction_checkpoint_claude_output_config")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "compaction_checkpoint_fallback_checkpoint")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "command_policy_cases"
      "CommandPolicyValidation")
      "command-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "command_policy_cases"
      "CommandPolicySandbox")
      "command-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "command_policy_cases"
      "CommandPolicyEnv")
      "command-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "command_isolation_cases"
      "CommandIsolation")
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "command_isolation_cases"
      "CommandIsolationSeatbelt")
      "command-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "command_policy_cases"
      "CommandPolicyOperatorUi")
      "command-policy" [Surface.operatorUi]
  , tagged (consumerCoverage
      "queue_deadline_conformance_cases"
      "QueueDeadlineConformanceCases")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "interrupt_queue_cases"
      "InterruptQueueCases"
      "Sequential cases bind Local and HTTP latch/drain, scheduled-origin eligibility, exact pending membership, notification bindings and replay. Native publisher timestamps do not bind modeled equal-time queue ordering. Background-process survival is a separate obligation.")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "session_control_cases"
      "SessionControlCases")
      "session-control" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "lifecycle_hook_effect_cases"
      "LifecycleHookEffectCases"
      "Production validate decides every case; application runs through the conformance test's in-memory Host, whose all-or-none apply mirrors Hooks.applyEffects. The DefraDB transaction host (request::hooks::native) is crate-private and is not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "lifecycle_hook_stage_cases"
      "LifecycleHookStageCases")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "lifecycle_hook_must_commit_cases"
      "LifecycleHookMustCommitCases")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "lifecycle_hook_rerun_cases"
      "LifecycleHookRerunCases"
      "Production TransactionFailure::rerun_requested classifies a ConfigClientError::rerun carried through production wrappers (anyhow context, HookError::Operation, CommitError::Other re-erased as on the plugin-job path, text rendering). The transaction owner's rerun loop itself is not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "lifecycle_hook_goal_resume_cases"
      "LifecycleHookGoalResumeCases"
      "Production RestageOnResume and BindResumedRoot decide every case on rows built from it; the Goal row is advanced by applying their writes. Core's request.create operation and the DefraDB host are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "lifecycle_hook_combine_cases"
      "LifecycleHookCombineCases")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "lifecycle_hook_gate_cases"
      "LifecycleHookGateCases")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "lifecycle_hook_queue_scope_cases"
      "LifecycleHookQueueScopeCases"
      "Production gate runs every case and production partition combines the answers through the conformance test's in-memory Host. The DefraDB transaction host, its revision read and the Goals listener (goal::lifecycle_hooks::AssignmentScope) are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "lifecycle_hook_delivery_cases"
      "LifecycleHookDeliveryCases")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "lifecycle_hook_admit_cases"
      "LifecycleHookAdmitCases")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "lifecycle_hook_grant_cases"
      "LifecycleHookGrantCases"
      "Production validate decides every representable case. A native grant rides on its create, so a grant on a document the result does not create cannot be expressed and is checked to be invalid in the model. The DefraDB host's relationship publication and the Goals listener (goal::lifecycle_hooks::TaskGoalAssignment) are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "lifecycle_hook_supersede_cases"
      "LifecycleHookSupersedeCases"
      "Production Selector::stale classifies and renders every case from row facts. The facts' extraction from AgentRequest rows (receipt verification, the parent read by document) and the retirement through the pending-supersede path are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "lifecycle_hook_goal_halt_cases"
      "LifecycleHookGoalHaltCases"
      "Production halt_verdict, with its RFC 3339 parsing, decides every case. The Goal write and outcome settlement PauseOnHalt stages for a pause verdict are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "lifecycle_hook_history_cases"
      "LifecycleHookHistoryCases"
      "Production history_permitted decides every case. The DefraDB host's refusal of an out-of-bound revision read before answering it is not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "fold_queue_contracts_fold_script"
      "Generated scripts drive the native claim transaction with an explicit verified-admission set, publish selected messages through the authored publication owner and finish turns, binding active, pending, selected and terminal rows and supersession pointers. The daemon's admission verification that produces that set is exercised natively, not generated.")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "fold_queue_contracts_handover_claim")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "fold_queue_contracts_publication_script")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "session_input_edit_cases"
      "SessionInputEditCases"
      "The model composes existing authority observations with the queue replacement owner and atomic terminal receipt. Signature validity and session ownership are supplied by their existing native owners; edits are direct client-signed commands, and the command introduces no new authority kind. Native binding must exercise these facts, exact replay and identity collision through the real reconciler. Distinct physical commands sharing an ID are an observed fail-closed barrier, including before application and after a receipt on another session; existing effects and receipts remain intact.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "session_input_edit_cases"
      "SessionInputEditCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "session_input_edit_client_observation_cases"
      "SessionInputEditClientObservationCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "fold_queue_contracts_management_replace"
      "Fifteen representable replacement cases exercise signed native replacement, rejection, immutable originals and physical slot order, each with an expired and a malformed TTL variant.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "fold_queue_contracts_management_expired_intake")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "fold_queue_contracts_management_unreached"
      "model-production-gap: the replacement API generates replacement request IDs and slots internally, and the remaining intake decisions (unsafe stream boundary, stale head observation, unadmitted, queued, foreign or agent-authored rows) have no served fixture; steering publication and the owned loop replay the composed intake.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "fold_queue_contracts_publication_unsafe_boundary"
      "The owned loop refuses intake at both unsafe streaming boundaries; the served writer scripts in fold_queue_contracts_publication_script bind signed pending rows, native intake, authored publication, cancellation, generation fencing and natural completion.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "fold_queue_contracts_publication_unsafe_boundary_unreached"
      "model-production-gap: the served writer fixture has no unsafe provider-stream boundary and the owned loop has no durable queue; fold_queue_contracts_publication_unsafe_boundary replays the refused intake through run_loop_stream.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "retry_selection_cases"
      "RetrySelectionCases")
      "request-lifecycle" [Surface.agentFacing]
  , tagged (consumerCoverage
      "fold_turn_input_cases"
      "FoldTurnInputCases")
      "request-lifecycle" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "interrupt_queue_cases"
      "InterruptQueueHttpOverlapCases"
      "Two observed-identity overlap scripts bind real HTTP transactions against embedded completion publication, including permitted pre-latch phantom survival and post-latch replay. These schedules do not prove arbitrary distributed serialization; exact native queue ordering remains unbound.")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "artifact_mode_meet_cases"
      "ArtifactModeMeetCases")
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "artifact_admission_cases"
      "ArtifactAdmissionCases"
      "External binaries must obey their declared effects and the host kernel must enforce the selected sandbox.")
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "artifact_spawn_cases"
      "ArtifactSpawnCases"
      "External binaries must obey their declared effects and the host kernel must enforce the selected sandbox.")
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "operator_base_freeze_cases"
      "OperatorBaseFreezeCases")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "workspace_path_capability_cases"
      "WorkspacePathCapabilityCases")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "identity_private_journal_policy"
      "PrivateJournalPolicy")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "workspace_liveness_cases"
      "WorkspaceLivenessCases"
      "Checks native filtered absence before exclusive release; complete workspace relationship publication remains separate.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "workspace_agent_selection_cases"
      "WorkspaceAgentSelectionCases"
      "Checks explicit workflow selection and action-plan replay; native workspace relationship publication remains separate.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "workspace_child_cases"
      "WorkspaceChildCases"
      "Canonical publication attaches the shared parent atomically; ACP1 permits raw admitted writers to create orphan rows.")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "workspace_quickstart_selection_cases"
      "WorkspaceQuickstartSelectionCases")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "workspace_callback_selection_visibility")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "pack_placement_reader_cases"
      "PackPlacementReaderCases")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "agent_target_access_access")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "native_route_cases"
      "NativeRouteCases")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "workspace_datastore_write_cases"
      "WorkspaceDatastoreWriteCases")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "workspace_root_access_cases"
      "WorkspaceRootAccessCases"
      "User publication fences on native Node update and commits the physical parent; raw direct creates remain outside the writer-fencing premise.")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "workspace_child_revocation_cases"
      "WorkspaceChildRevocationCases"
      "Documents the completeness limit after explicitly granting a child owner native relationship deletion.")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "workspace_admission_cases"
      "WorkspaceAdmissionCases"
      "Exercises competing canonical exclusive admissions; native conflict fencing needs central validation.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "workspace_cleanup_cases"
      "WorkspaceCleanupCases"
      "Cleanup claims the parent with native update permission after checking the canonical binding inventory.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "workspace_publication_cases"
      "WorkspacePublicationCases"
      "Checks duplicate publication after revocation and a stale retry after cleanup has begun.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "workspace_quickstart_cases"
      "WorkspaceQuickstartCases"
      "Covers user-signed repository publication, Agent visibility, outsider isolation and revocation; host provisioning follows reservation.")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_completion_access"
      "Exercises selected-Agent reads using a test-only native binding; product workspace relationships and remaining readers require separate coverage.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_and_file_requests_completion_actor"
      "Tests selection of the held completion writer; workspace read isolation and cross-Agent relationship publication remain separate.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_root_completeness"
      "Checks root projection for incomplete views; an ACL-bound authoritative root publication contract remains separate.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "mailbox_access_claimed_recipient_served"
      "Connects a native User-authored client claim to the mailbox tool and recipient relationship, and after restart to the runtime notice; unattended provenance remains unavailable.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "mailbox_access_claimed_recipient_unreached"
      "model-production-gap: no served fixture commits a client claim without its verified admission; mailbox_access_claimed_recipient_served replays the unadmitted lifecycle before its claim.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_access_claim_visibility")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_access_claim_tool_context")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "mailbox_access_sweep_actors"
      "Exercises held-Agent reconciliation; newly configured recipients need authenticated source provenance.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "mailbox_access_notice_publication"
      "delegated_http_claim_routes_mailbox_to_immediate_signer publishes a notice for a missing request, an admitted but unclaimed request and a claimed request; acknowledgement replay after the claim remains scenario evidence.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "mailbox_access_native"
      "Tests native mailbox publication and recipient revocation; authenticated producer provenance and durable runtime notices remain separate obligations.")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "revoke_follows_grant_steps")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "node_file_requests_access"
      "The retained FileInventoryRequest schema has no production publisher or consumer; this test covers native document authority, not a live peer flow.")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_and_file_requests_callback_actor"
      "Checks execution-context binding only. Native workspace policy, relationship publication and authorization retain their separate owners.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_input_edit_native_discovery")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "session_input_edit_native_claim_cases"
      "SessionInputEditNativeClaim")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "identity_private_journal_policy"
      "PrivateJournalSigner")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "file_review_subscription_cases"
      "FileReviewSubscriptionCases")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "file_review_routes")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "scope_node_route_cases"
      "ScopeNodeRouteCases")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_order_window_cases"
      "FileInventoryOrderWindowCases"
      "Binds canonical metadata observations and resource ordinals to native projection. Database-backed snapshot/signature binding and installed review remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_order_resolution_cases"
      "FileInventoryOrderResolutionCases"
      "Binds canonical metadata observations and resource ordinals to native projection. Database-backed snapshot/signature binding and installed review remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_order_comparison_cases"
      "FileInventoryOrderComparisonCases"
      "Binds per-request source coverage to selected physical scopes. Verified no-change producer observations and installed per-turn totals remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_order_comparison_cases"
      "FileInventoryOrderComparisonCases"
      "Binds canonical metadata observations and resource ordinals to native projection. Database-backed snapshot/signature binding and installed review remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_order_entry_cases"
      "FileInventoryOrderEntryCases"
      "Binds canonical metadata observations and resource ordinals to native projection. Database-backed snapshot/signature binding and installed review remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_direct_reader_cases"
      "FileInventoryDirectReaderCases"
      "Drives actual operator reads and bounded pages under the authenticated reader. HTTP extraction and installed consumption remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_fork_cases"
      "FileInventoryForkCases"
      "Uses the native fork owner and source snapshot reader to preserve selected physical origins while excluding later parent work. Installed acceptance remains a separate obligation.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_reader_cases"
      "FileInventoryReaderCases"
      "Refines public-root visibility, exact physical binding, explicit projection read permission, revocation and reauthorization for the authenticated direct reader in one native source snapshot. Installed consumption remains a separate obligation.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "file_inventory_source_projection_cases"
      "FileInventorySourceProjectionCases"
      "Refines source projection scope, terminal coverage, path coherence, authenticated native owners and unsigned bounded pages. Remaining canonical admission, fork and publication refinements stay separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "schema_argument_repair_cases"
      "SchemaArgumentRepairCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "invalid_tool_progress_cases"
      "InvalidToolProgressCases")
      "completion-retry" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "repeated_tool_failure_cases"
      "RepeatedToolFailureCases")
      "completion-retry" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "self_config_selection_cases"
      "SelfConfigSelectionCases")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_model_slot_cases"
      "PluginModelSlotCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_budget_cases"
      "PluginResourceCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_network_cases"
      "PluginNetworkCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_call_access_cases"
      "PluginCallAccessCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_invocation_cases"
      "PluginInvocationCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_publication_cases"
      "ToolPolicy.PluginPublication")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_receipt_cases"
      "PluginReceiptCases")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_resource_child_admission_cases"
      "PluginChildAdmissionCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_tool_effect_cases"
      "PluginEffectCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_tool_effect_terminal_cases"
      "PluginEffectTerminalCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_tool_effect_accounting_cases"
      "PluginEffectAccountingCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_tool_call_cases"
      "PluginToolCallCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_job_admission_cases"
      "PluginJobAdmissionCases")
      "plugin-jobs" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_job_adoption_cases"
      "PluginJobAdoptionCases")
      "plugin-jobs" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_job_control_cases"
      "PluginJobControlCases")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_job_key_cases"
      "PluginJobKeyCases")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_job_authority_cases"
      "PluginJobAuthorityCases")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_job_wire_cases"
      "PluginJobWireCases")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_job_wire_cases"
      "PluginJobWireCases")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "plugin_job_lineage_cases"
      "PluginJobLineageCases"
      "Rows decode strictly against DurableLineage.openingRequest. The client consumers that drive the native opening-request owner are pending.")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_job_lineage_cases"
      "PluginJobLineageCases")
      "plugin-jobs" [Surface.operatorCli]
  , tagged (consumerCoverage
      "plugin_job_lineage_cases"
      "PluginJobLineageCases")
      "plugin-jobs" [Surface.operatorUi]
  , tagged (consumerCoverage
      "plugin_job_recovery_cases"
      "PluginJobRecoveryCases")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_job_wake_cases"
      "PluginJobWakeCases")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "restart_disposition_cases"
      "RestartDispositionCases")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r6_backgrounding_cases"
      "R6BackgroundingCases")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_data_cases"
      "PluginDataCases")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_scratch_cases"
      "PluginScratchCases")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_resource_host_import_cases"
      "PluginHostImportCases")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "plugin_calls_route_cases"
      "PluginCallsRouteCases")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "self_config_cases"
      "SelfConfigCases")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "tool_timeout_cases"
      "ToolTimeoutCases")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "workspace_path_alias_admission")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_claimed_publication_cases"
      "GoalClaimedPublicationCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_first_request_binding_cases"
      "GoalFirstRequestBindingCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_continuation_assignment_cases"
      "GoalContinuationAssignmentCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_request_assignment_contracts_history")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_request_assignment_contracts_persisted_history")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "session_message_goal_input_cases"
      "SessionMessageGoalInputCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_delivery_input_cases"
      "GoalDeliveryInputCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_input_binding_cases"
      "GoalInputBindingCases")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage
      "goal_fresh_session_input_cases"
      "GoalFreshSessionInputCases")
      "durable-goals" [Surface.operatorCli]
  , tagged (consumerCoverage
      "goal_request_assignment_cases"
      "GoalRequestAssignmentCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_wake_receipt_cases"
      "GoalWakeReceiptCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_wake_coalescing_cases"
      "GoalWakeCoalescingCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_wake_ancestry_cases"
      "GoalWakeAncestryCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_tool_context_cases"
      "GoalToolContextCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_request_scope_cases"
      "GoalRequestScopeCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_request_ancestry_cases"
      "GoalRequestAncestryCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_queue_assignment_contracts_fold_decision")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_queue_assignment_contracts_steer_decision")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "durable_observation_cases"
      "DurableObservationCases")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_job_commit_kill_cases"
      "PluginJobCommitKillCases")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "client_process_claim_cases"
      "ClientProcessClaimCases")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "client_process_receipt_cases"
      "ClientProcessReceiptCases")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "client_control_grant_cases"
      "ClientControlGrantCases")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "client_signed_control_cases"
      "ClientSignedControlCases")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "process_stop_action_cases"
      "ProcessStopActionCases")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "background_process_identity_cases"
      "BackgroundProcessIdentityCases")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "plugin_target_resolution_cases"
      "PluginTargetResolutionCases")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_request_head_contracts_selection")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_request_head_contracts_membership")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_edit_contracts_edit")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_client_control_cases"
      "GoalClientControlCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_client_start_cases"
      "GoalClientStartCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_client_receipt_cases"
      "GoalClientReceiptCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_observation_cases"
      "GoalObservationCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_observation_cases"
      "GoalObservationCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_session_observation_cases"
      "GoalSessionObservationCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_session_observation_cases"
      "GoalSessionObservationCases")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage
      "goal_session_observation_cases"
      "GoalSessionObservationCases")
      "durable-goals" [Surface.operatorUi]
  , tagged (followUpCoverage
      "goal_session_fold_cases"
      "GoalSessionFoldCases"
      "The desktop Goal view test replays these folds from GoalSessionObservationConformance.lean; nothing reads the snapshot's copy.")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage
      "plugin_approval_cases"
      "PluginApprovalCases")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_objective_whitespace_codepoints"
      "GoalObjectiveWhitespace")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_operator_resume_cases"
      "GoalOperatorResumeCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_operator_resume_cases"
      "GoalOperatorResumeCases")
      "durable-goals" [Surface.operatorCli]
  , tagged (consumerCoverage
      "goal_operator_resume_on_cases"
      "GoalOperatorResumeOnCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_config_reactivation_cases"
      "GoalConfigReactivationCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_reset_resume_cases"
      "GoalResetResumeCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "restart_disposition_cases"
      "RestartDispositionCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "restart_disposition_cases"
      "RestartDispositionCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "r6_backgrounding_cases"
      "R6BackgroundingCases"
      "The scope comparator preserves absent-versus-empty behavior for retained history. Native signed admission rejects a requester-less local-self request before any hook effect; accepted hook envelopes use a real admitted requester, and a forged requester fails against its immutable physical header.")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "r6_backgrounding_cases"
      "R6BackgroundingCases"
      "Binds all seven completion_continuation_owner cases through accepted native tool output, durable notification and redrive owners. Parent completion is an imported model observation; this consumer does not establish the parent terminalization transition or the separate continuation-publication family.")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "r6_backgrounding_cases"
      "R6BackgroundingCases"
      "The generated caller-interrupt observer-completion case derives the completed foreground wait through the tool owner and the unchanged Running background process through the observation owner. The native consumer binds both states and the persisted structured result. This deterministic observer path does not decide the competing request-cancellation winner.")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "r6_backgrounding_cases"
      "R6BackgroundingCases"
      "The generated caller-interrupt dispatch case supplies the accepted Running foreground wait's cancelled state and unchanged background state; the native test separately asserts exact canonical cancellation text. The observer case supplies the only structured alternative. This finite race check does not establish a deterministic winner or scheduler fairness.")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "restart_disposition_cases"
      "RestartDispositionCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r6_backgrounding_cases"
      "R6BackgroundingCases")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "composed_invariant_witnesses"
      "ComposedInvariantWitnesses"
      "Covers four persisted tool recovery/cancellation outcomes through canonical accepted admission and physical request/tool rows. Full composed request, admission and clock traces still need their runtime owners; fixture path assertions are not replay.")
      "composed-invariants" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r6_background_theorem_witnesses"
      "BackgroundBudgetBoundedTheoremWitness")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r4c_background_work_cases"
      "R4cBackgroundWorkCases")
      "background-tools" [Surface.operatorUi]
  , tagged (followUpCoverage
      "r4c_background_work_cases"
      "R4cBackgroundWorkCases"
      "#1571: replace the retired state/registry dispatch consumer with canonical_source_reconstruction over physical tool segments; migrate the generated witness schema. Generic paging coverage below is unchanged.")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "tool_output_paging_cases"
      "ToolOutputPagingCases")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "interrupt_disposition_cases"
      "InterruptDispositionCases")
      "background-tools" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "transcript_conformance_cases"
      "TranscriptConformanceCases"
      "Native owners cover ordering, model-scripted parallel completion, pair closure, and abandonment. Full coverage needs transaction fault injection for lost-ack duplicate delivery replay and a permissive orphan-result publication owner.")
      "transcript" [Surface.agentFacing]
  , tagged (consumerCoverage
      "transcript_conformance_cases"
      "TranscriptConformanceCases")
      "transcript" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "identity_permission_cases"
      "IdentityPermissionCases"
      "Checks native Defra ACP using the selected agent DID after explicit node-scoped resolution. Unknown-DID and duplicate-DID selector rejection require the canonical registry owner; no test-local ID map remains.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "identity_permission_cases"
      "IdentityPermissionCases")
      "identity-permission" [Surface.api]
  , tagged (consumerCoverage
      "identity_household_cases"
      "IdentityHouseholdCases")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "identity_derivation_cases"
      "IdentityDerivationCases")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "identity_contracts"
      "IdentityContracts"
      "Route through the canonical DID registry and exercise rejection before permission checks. The removed synthetic global-ID map did not exercise runtime routing.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "canonical_worker_capacity_cases"
      "CanonicalWorkerCapacityCases"
      "Bind modeled acquire, full-capacity refusal and release to the slot-owned Rust capacity scheduler. No request parks a worker while waiting on another session.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "canonical_execution_gate_cases"
      "CanonicalExecutionGateCases"
      "Generated native scripts bind transactional lease, publication, recovery and tool owners. Title scripts bind signed parent-only admission, own claim/begin, renewal, Complete/Partial/retracted audit closure, retry coordinates, terminal-parent isolation and purpose denial. Rows marked nativeGap retain explicit replication or unmaterialized-input premises; trace summaries and finite cases do not establish the application's universal invariants in Rust.")
      "canonical-output" [Surface.runtimeInternal, Surface.agentFacing]
  , tagged (consumerCoverage
      "canonical_spawned_target_rejection_cases"
      "CanonicalSpawnedTargetRejectionCases")
      "canonical-output" [Surface.runtimeInternal, Surface.agentFacing]
  , tagged (consumerCoverage
      "canonical_dispatch_observation_cases"
      "CanonicalDispatchObservationCases")
      "canonical-output" [Surface.runtimeInternal, Surface.agentFacing]
  , tagged (consumerCoverage
      "canonical_dispatch_observation_cases"
      "CanonicalDispatchObservationCases")
      "canonical-output" [Surface.runtimeInternal, Surface.agentFacing]
  , tagged (consumerCoverage
      "canonical_dispatch_observation_cases"
      "CanonicalDispatchObservationCases")
      "canonical-output" [Surface.runtimeInternal, Surface.agentFacing]
  , tagged (consumerCoverage
      "canonical_dispatch_observation_cases"
      "CanonicalDispatchObservationCases")
      "canonical-output" [Surface.runtimeInternal, Surface.agentFacing]
  , tagged (consumerCoverage
      "title_text_cases"
      "TitleTextCases")
      "canonical-output" [Surface.operatorUi, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "title_admission_join_cases"
      "TitleAdmissionJoinCases"
      "Model-derived row observation and independent title-world inputs exercise admitted direct own-lease claim, exact physical/logical row binding, active normal queue preservation, unavailable retry and invalid preclaim facts. The executable join calls the existing title activation/claim owner and checks non-vacuity, but its exported projections do not serialize a full application trace or bind native materialization, renewal, recovery or output capture.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "canonical_payload_presentation_cases"
      "CanonicalPayloadPresentationCases"
      "The four generated cases bind presented payload-field bytes and missing-dependency failure to native canonical reconstruction; stored-byte quantities remain Lean expectations. Provider serialization, estimator output, and compaction threshold remain unbound here; the real serialized-request/threshold experiment is native.external-projected-request-threshold.")
      "canonical-output" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "terminal_diagnostic_presentation_cases"
      "TerminalDiagnosticPresentationCases"
      "Eight valid generated inputs bind exact native presentation ranges, literals, and rendered bytes. Two invalid UTF-8 inputs are rejected at the adapter representation boundary via String::from_utf8; the production presentation helper accepts typed strings. This pure binding does not establish capture of uncommitted process bytes, terminal CAS authority, crash recovery, or provider continuation; those remain obligations of their existing owners.")
      "canonical-output" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "terminal_diagnostic_replay_cases"
      "TerminalDiagnosticReplayCases")
      "canonical-output" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "compaction_projection_join_cases"
      "CompactionProjectionJoinCases"
      "Model-derived projection/estimate/reduction and rebuilt-admission observations bind exact input identities, failure propagation and threshold/output-capacity boundaries. Provider projection and estimation are explicit fallible native premises, not modeled serializers. Native adapters and the real serialized-request experiment remain pending; controlled observations are not native consumer coverage.")
      "compaction" [Surface.agentFacing]
  , tagged (followUpCoverage
      "compaction_canonical_projection_cases"
      "CompactionCanonicalProjectionCases"
      "Concrete canonical records and selected identities exercise the composed reconstruction/projection/reduction entry. Compare full native messages at initial and checkpoint-rebuild callbacks, and reject missing or loading dependencies without shortened history. The native serializer and ACP-authorized snapshot remain external premises.")
      "compaction" [Surface.agentFacing]
  , tagged (followUpCoverage
      "repaired_projection_admission_cases"
      "RepairedProjectionAdmissionCases"
      "Fresh provider-view admission reprojects and re-estimates supplied repaired input; generated observations cover failures, equality, over-threshold and output-capacity checks. This does not model the repair transformation or establish native consumer coverage.")
      "compaction" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "canonical_output_projection_cases"
      "CanonicalOutputProjectionCases"
      "The two published mixed/all-opaque reasoning cases bind present_message to renderedKinds. This binds stateless reasoning presentation, not projection selection, ACP or genesis validation.")
      "canonical-output" [Surface.agentFacing, Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "canonical_output_projection_cases"
      "CanonicalOutputProjectionCases"
      "The two published mixed/all-opaque reasoning cases bind render_reasoning_text to renderedKinds. This binds the stateless per-event rendering function, not stream-event delivery or reconnect behavior.")
      "canonical-output" [Surface.agentFacing, Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "reasoning_audit_cases"
      "ReasoningAuditCases"
      "Generated open, partial, retracted, continued-signature and gap observations bind exact native dense-prefix reconstruction, including subtype and bytes. They do not establish provider-event capture, durable write admission, ACP, or full replica completeness beyond the observed prefix.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "reasoning_delta_cases"
      "ReasoningDeltaCases"
      "Generated fragment cases drive the native accumulator across reasoning identities, signed boundaries, text boundaries and UTF-8 bytes.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "reasoning_signature_cases"
      "ReasoningSignatureCases"
      "Generated typed cases bind published-header signature agreement to retained signature bytes, including provider inline-signature rejection without a retained stream and authored inline-signature allowance. Invalid UTF-8 is rejected at the byte-to-String adapter boundary because native OutputSegment payload is typed String; this does not establish provider-event capture, durable write admission, or ACP.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "auxiliary_output_cases"
      "AuxiliaryOutputCases"
      "Generated compaction/fallback cases bind claimed append denial, own begin/append, Complete or Partial exact close, stale-writer close replay denial, and nonempty auxiliary publication denial to native request/output owners. Empty-assistant publication is Lean-only because the native encoder rejects that shape. This does not cover title ownership, recovery after crash, or every provider cancellation path.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "auxiliary_output_cases"
      "AuxiliaryOutputCases"
      "Generated compaction/fallback cases drive the non-Claude auxiliary sink with typed reasoning, signature, encrypted and redacted observations, then compare durable bytes, exact closure, private audit reconstruction and absent public live/message output to Lean. This does not cover crash durability before sink acknowledgement, title ownership, recovery after crash, or every provider cancellation path.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "auxiliary_output_cases"
      "AuxiliaryOutputCases"
      "Generated compaction/fallback records bind exact private audit bytes and absent public live projection to native reconstruction. This is a read-only projection check; it does not establish provider-event capture, durable write admission, recovery transactions, title ownership, or ACP.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "claude_prompt_assembly_wire_start"
      "The four generated wire-start cases bind initial signature plus later delta, absent initial signature, ordered rejection of later thinking, and malformed signature type at the native SSE parser. The native test compares final content/error, not intermediate modeled steps or durable output capture.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "workspace_command_grant_answers"
      "WorkspaceCommandGrantAnswers")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "workspace_command_grant_lookups"
      "WorkspaceCommandGrantLookups")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "workspace_command_grant_revocations"
      "WorkspaceCommandGrantRevocations")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "workspace_permission_write_cases"
      "WorkspacePermissionWriteCases")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "workspace_permission_write_cases"
      "WorkspacePermissionWriteCases"
      "Generated transitions drive persisted selections with policy fingerprints on one Task ID, stale revisions, missing reviewers, retained references, no-op writes and exhausted revisions. Concurrent locking and filesystem failure handling need adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "access_review_reply_decision"
      "Generated JSON replies bind exact invocation IDs, complete decisions and UTF-8 byte limits. Context cases bind required-envelope limits and whole-entry selection in newest-first order, including fixed-width overflow boundaries. Traces bind denial windows and scoped routing. Native encoders must supply honest wire costs; signed review execution, physical request binding, cancellation, tool and hook exclusion, and reviewer accuracy require adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "access_review_denial_trace"
      "Generated JSON replies bind exact invocation IDs, complete decisions and UTF-8 byte limits. Context cases bind required-envelope limits and whole-entry selection in newest-first order, including fixed-width overflow boundaries. Traces bind denial windows and scoped routing. Native encoders must supply honest wire costs; signed review execution, physical request binding, cancellation, tool and hook exclusion, and reviewer accuracy require adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "access_review_review_route"
      "Generated JSON replies bind exact invocation IDs, complete decisions and UTF-8 byte limits. Context cases bind required-envelope limits and whole-entry selection in newest-first order, including fixed-width overflow boundaries. Traces bind denial windows and scoped routing. Native encoders must supply honest wire costs; signed review execution, physical request binding, cancellation, tool and hook exclusion, and reviewer accuracy require adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "access_review_completion_acceptance"
      "Generated JSON replies bind exact invocation IDs, complete decisions and UTF-8 byte limits. Context cases bind required-envelope limits and whole-entry selection in newest-first order, including fixed-width overflow boundaries. Traces bind denial windows and scoped routing. Native encoders must supply honest wire costs; signed review execution, physical request binding, cancellation, tool and hook exclusion, and reviewer accuracy require adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "access_review_context_selection"
      "Generated JSON replies bind exact invocation IDs, complete decisions and UTF-8 byte limits. Context cases bind required-envelope limits and whole-entry selection in newest-first order, including fixed-width overflow boundaries. Traces bind denial windows and scoped routing. Native encoders must supply honest wire costs; signed review execution, physical request binding, cancellation, tool and hook exclusion, and reviewer accuracy require adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "access_review_task_cases"
      "AccessReviewTaskCases"
      "Generated Task documents bind node, task, agent, operator-selected policy text, eligibility and excluded execution effects. Store lookup, template evaluation, policy fingerprint integrity, concurrent edits and provider behavior remain native adapter boundaries.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_command_root_selection"
      "Canonical host paths drive the production writable-root selection. A macOS execution regression checks real writes inside the project, rejection in its sibling and Full access restoring the configured root; other platforms retain the existing sandbox-unavailable barrier.")
      "tool-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "operator_path_cases"
      "OperatorPathCases"
      "Generated decisions drive the shared path guard with real files, hard links, nested approval queues, missing targets and failed filesystem inspection. File-tool regressions verify that both reads and writes reject operator aliases before touching contents. Filesystem handle identity and concurrent external namespace changes remain native premises.")
      "tool-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "workspace_permission_cases"
      "WorkspacePermissionCases"
      "Generated decisions bind workspace isolation, configured ceilings, action/revision grants and automatic-review fallback to the native routing owner. Canonical host paths, command fingerprints, sandbox enforcement and reviewer accuracy require adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_service_tier_cases"
      "PromptAssemblyServiceTierCases"
      "Generated decisions cover every provider family with unset, Standard and Fast selections. Native validation rejects unsupported carriers; supported cases compare the actual provider request body. Provider account/model eligibility and delivered speed are external.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_responses_storage_cases"
      "PromptAssemblyResponsesStorageCases"
      "The generated cases drive loop_config and the loop's own ProviderInputCounter projection, so wire params and accounting are checked together. Premise: reasoning unset and a non-empty preamble. The OAuth transports' own HTTP store patch is fenced by its unit tests, not by this case family.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_responses_effort_cases"
      "PromptAssemblyResponsesEffortCases"
      "The generated cases drive loop_config and the loop's own ProviderInputCounter projection, comparing the projected reasoning.effort. Premise: OpenAiCompatible on the Responses wire; kind and wire are not modelled, so these cases cannot catch a wrong kind/wire guard (pinned by inference_setup::tests::sent_reasoning_effort_leaves_other_kinds_and_wires_alone).")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "compaction_reducer_cases"
      "CompactionReducerCases"
      "Strip/provider-view and immutable publication-gate cases are executable Lean witnesses. The native compaction owner must migrate its reducer gate and checkpoint execution before this is consumer coverage.")
      "compaction" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "protected_replay_compaction_cases"
      "ProtectedReplayCompactionCases"
      "Native replay association and reduction ceilings preserve the entire captured prefix of required reasoning. A nonempty required set refuses summarization with CannotFit; an empty set preserves the ordinary retention policy. Retirement of older requirements remains deferred. Generated cases bind modeled assistant blocks to native ceiling and split selection; the summary-provider refusal has separate native coverage.")
      "compaction" [Surface.agentFacing]
  , tagged (followUpCoverage
      "compaction_cursor_cases"
      "CompactionCursorCases"
      "The cursor fixtures share the migrated immutable compaction contract; the native bridge has not yet been regenerated.")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "routing_affinity_cases"
      "RoutingAffinityCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "current_input_cases"
      "CurrentInputCases"
      "Binds canonical header classification and selection to CurrentInput. Full reconstructed-history regression additionally covers retaining a current-request tool result; arbitrary imported-seed reachability is not claimed.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "wake_input_cases"
      "WakeInputCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_sanitize_cases"
      "PromptAssemblySanitizeCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_layer_cases"
      "PromptAssemblyLayerCases")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "prompt_assembly_repair_cases"
      "PromptAssemblyRepairCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_budget_cases"
      "PromptAssemblyBudgetCases")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "prompt_assembly_turn_budget_cases"
      "PromptAssemblyTurnBudgetCases")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "prompt_assembly_compaction_trigger_cases"
      "PromptAssemblyCompactionTriggerCases")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "prompt_assembly_retention_cases"
      "PromptAssemblyRetentionCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_claude_map_cases"
      "PromptAssemblyClaudeMapCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_claude_body_cases"
      "PromptAssemblyClaudeBodyCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "claude_prompt_assembly_attribution")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "claude_prompt_assembly_cache_placement")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "claude_prompt_assembly_origin")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "claude_prompt_assembly_field_decision")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "claude_prompt_assembly_field_row")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "prompt_assembly_claude_stream_cases"
      "PromptAssemblyClaudeStreamCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_claude_thinking_stream_cases"
      "PromptAssemblyClaudeThinkingStreamCases"
      "Drives the native Claude Messages SSE parser and compares cumulative provisional text and sealed signed/redacted content. The owned loop's persistence and provider continuation are not exercised here.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_claude_thinking_stream_cases"
      "PromptAssemblyClaudeThinkingStreamCases"
      "Drives modeled nonempty thinking-start text through the native parser and canonical accumulator, checking exact sealed content. Owned-loop persistence and provider continuation remain unbound.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_claude_replay_cases"
      "PromptAssemblyClaudeReplayCases"
      "Reconstructs native assistant blocks and checks exact Claude body replay or fail-closed error, including empty text and unsupported assistant image. Actual HTTP transport and request budgeting are not exercised here.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_claude_replay_cases"
      "PromptAssemblyClaudeReplayCases"
      "Drives the streamed-signed-redacted-tool case through the owned loop: its canonical blocks become turn-1 SSE, the persisted message must retain their exact grouping and payloads, and the turn-2 capture must equal the modeled replay. Other replay cases and the thinking-stream family remain unbound through the owned loop.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_reasoning_suffix_cases"
      "PromptAssemblyReasoningSuffixCases"
      "Binds native two-phase selection (provenance suffix, stage assembly, turn location and capture-derived admissible suffix) over the model's row cases and every step of the replay scenarios (compaction, repair, strip-and-retry, tool and issuer changes, interruption, restart, Claude signatures and redaction, Responses ciphertext). Native flattening of real provider bodies is bound by gents-loop replay_frontier tests, not by these cases.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_replay_shape_cases"
      "PromptAssemblyReplayShapeCases"
      "Binds original-index block shaping and misaligned-sidecar rejection. Complete compaction-history carriage is exercised through native request assembly tests.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_replay_prefix_cases"
      "PromptAssemblyReplayPrefixCases"
      "Binds the two capture checks and their agreement with leading-reasoning removal on flat items. Provider acceptance itself is an external premise documented on ReplayFrontier.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_claude_checkpoint_cases"
      "PromptAssemblyClaudeCheckpointCases"
      "Models source-tagged selected assistant occurrences, the exact reduction split that keeps a pending tool round out of a summary, and the strict Claude codec. Replay selection is PromptAssemblyReasoningSuffixCases; this model does not prove tag issuance (#1693).")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage "provider_tool_identity_cases" "ProviderToolIdentityCases")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_assistant_order_cases"
      "PromptAssemblyAssistantOrderCases"
      "Checks the mode-selected native assistant-content ordering helper. It does not establish that every provider dispatch selects the correct mode.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "prompt_assembly_mode_sanitize_cases"
      "PromptAssemblyModeSanitizeCases"
      "Checks the composed native provider-view sanitizer on finite coherent, unique-call-id witnesses, and that the tagged association view is the native-order view whose send-boundary ordering is that composed view. General native reachability and arbitrary transcript premises remain outside this fence.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "rendered_capture_cases"
      "RenderedCaptureCases"
      "Drives provider-send ordering with scripted sink outcomes. Durable fresh/idempotent/conflicting bindings are observed separately by the real sink consumer; no test-side store model remains.")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "RenderedCaptureCases")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contract_cases_rendered_capture_storage_resolution"
      "The native consumer checks full and delta codec reconstruction with an injected base and witness. It does not exercise DefraDB base lookup, authorization, or provider-send ordering.")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "durable_reduction_cases"
      "DurableReductionCases"
      "Checks durable create/load/conflict and capture citations. The exported send_permitted fence is not exercised: validate actual provider dispatch against durable reduction facts through the owned completion loop.")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contract_cases_rolling_compaction_commit")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contract_cases_reduction_engine_outcome")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "budget_rehydration_cases"
      "BudgetRehydrationCases"
      "Drives absent, zero and positive pinned limits with inference/compaction rows through the real physical-request query and ledger factory. Full process restart and InferenceCall write ownership remain separate boundaries.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "RenderedCaptureKeyCases")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "CaptureScopeCases")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "CaptureOrderCases")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "RenderedCaptureCases")
      "rendered-capture" [Surface.operatorCli]
  , tagged (consumerCoverage
      "rendered_capture_cases"
      "RenderedCaptureCases")
      "rendered-capture" [Surface.operatorUi]
  , tagged (consumerCoverage "family" "event_delivery_watcher_transitions")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "event_delivery_watcher_releases")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "event_delivery_event_source_siblings")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "EventDelivery.substrate"
      "Persist/depersist and subscription enqueue/drop/consume are database and transport observations. The executable relation checks their input witnesses; native delivery families observe emitted requests and local suppression, not an exposed transport queue multiset.")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "event_delivery_source_contracts")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "event_delivery_watcher_convergence")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "event_delivery_event_source_convergence")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "mcp_health_cases"
      "MCPHealthCases")
      "mcp-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "mcp_health_cases"
      "MCPHealthCases")
      "mcp-health" [Surface.operatorCli]
  , tagged (consumerCoverage
      "mcp_health_cases"
      "MCPHealthCases")
      "mcp-health" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "tool_execution_cancel_cause_vocabulary")
      "interrupt-and-cancel" [Surface.operatorUi]
  , tagged (consumerCoverage
      "state_machine"
      "Request")
      "interrupt-and-cancel" [Surface.operatorUi]
  , tagged (followUpCoverage
      "workspace_cases"
      "WorkspaceCases"
      "Replay these cases through the production Workspace lifecycle owner after its canonical node fields migrate; the deleted test-local state table was not implementation coverage.")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "workspace_binding_cases"
      "WorkspaceBindingCases"
      "Replay these cases through the production Workspace binding admission owner after node-field migration; do not restore the deleted test-local binding predicate.")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "callback_cases"
      "CallbackCases"
      "Generated journal-prefix observations exercise the actual journal owner. Invocation-state legality, denied execution and result-emission ordering need the callback executor consumer; the removed Rust predicate copy did not establish them.")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "callback_retry_cases"
      "CallbackRetryCases")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "callback_recovery_cases"
      "CallbackRecoveryCases")
      "isolated-workspaces" [Surface.runtimeInternal]

  , tagged (consumerWithFollowUp "runtime_reconcile_cases" "RuntimeReconcileCases"
      "Pending the L3 test rename. Exercises explicit request/session agent binding. Readiness, atomic admission and generation lifetime remain router-owner obligations.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp "request_input_cases" "RequestInputCases"
      "Canonical input serde only. Signed canonical_input_fields/bytes, context whitelist, title materialization and verified goal receipt checks need the migrated admission/signing owners.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "request_input_cases" "RequestInputCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "request_input_cases" "RequestInputCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "session_reply_preview_cases" "SessionReplyPreviewCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp "session_document_projection_cases" "SessionDocumentCases"
      "Generated pending timeline cases bind physical identity, admission order, same or following request anchors, hidden or missing anchor fallback, tool-first replication, orphan groups and overlay ownership to the shared protocol ordering owner. Durable request lookup and authored-message suppression remain bridge-owner obligations.")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage "session_document_selection_cases" "SessionDocumentCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "session_document_fork_cases" "SessionDocumentCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "session_document_archive_cases" "SessionDocumentCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "session_document_retry_cases" "SessionDocumentCases"
      "Canonical session DB/lifecycle and fork tests specify target behavior; the generated interned-ID selection/projection/retry/fork rows still need real-owner adapters, including exact physical references and transactional freshness. No fixture-local state machine may substitute.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "background_wake_row_cases" "BackgroundWakeRowCases"
      "Consume exact physical-parent and cross-requester authoritative rows through background publication after its owner migrates. Existing DB wake tests do not establish every generated row verdict.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp "family" "configuration_scope_context_bounds"
      "Resolve same-label documents through the real owner-qualified context/inference registry; do not rebuild Lean lookup in tests.")
      "apply-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp "family" "configuration_scope_default_agent_publication"
      "Resolve same-label documents through the real owner-qualified context/inference registry; do not rebuild Lean lookup in tests.")
      "apply-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp "family" "configuration_scope_default_replacement"
      "Resolve same-label documents through the real owner-qualified context/inference registry; do not rebuild Lean lookup in tests.")
      "apply-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp "family" "configuration_scope_oauth_account"
      "Resolve same-label documents through the real owner-qualified context/inference registry; do not rebuild Lean lookup in tests.")
      "apply-reconcile" [Surface.runtimeInternal]
  , tagged (followUpCoverage "discovery_scope_cases" "DiscoveryScopeCases"
      "Exercise backend-owner and credential-scoped catalog selection in the production discovery owner, including shared/OAuth and foreign-owner rejection.")
      "apply-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "initialization_initial_inference")
      "apply-reconcile" [Surface.operatorCli]
  , tagged (followUpCoverage "event_group_clock_cases" "EventGroupClockCases"
      "Drive the existing durable group clock owner after typed trigger/callback identity migration; preserve first-seen and quiescence across restart.")
      "triggers" [Surface.runtimeInternal]
  , tagged (followUpCoverage "event_group_capture_cases" "EventGroupCaptureCases"
      "Drive captured ordered input and typed origin through the callback capture owner; quiesced groups must reject capture.")
      "triggers" [Surface.runtimeInternal]
  , tagged (followUpCoverage "callback_transition_cases" "CallbackTransitionCases"
      "Drive the callback executor's claim/run/succeed/fail/denial operations and observe exact input/origin/journal/emission; journal-prefix-only checks cannot establish lifecycle coverage. The interrupt case's journal and retry refusal are executed through callback_recovery_cases.")
      "triggers" [Surface.runtimeInternal]

  , tagged (consumerCoverage "discovery_guidance_cases" "DiscoveryGuidanceCases")
      "prompt-assembly" [Surface.agentFacing]
  ]

def followUpHookCoverage : List CoverageEntry :=
  [ tagged (consumerCoverage
      "follow_up_hook"
      "PromptAssembly.providerInput.sanitizeLoadedHistory")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (boundaryCoverage
      "follow_up_hook"
      "Compaction.providerViewAppend.uniqueCallIdsChecked"
      "Native provider transport codecs must preserve the content and physical-document identity represented by the checked per-turn row projection.")
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

def externalPremiseCoverage : List CoverageEntry :=
  [ tagged (boundaryCoverage "premise" "boundary.inference-slots.running-row-derived"
      "The database schema stores running InferenceCall rows rather than a denormalized held-slot document.") "inference-call" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.fleet-slot-accounting.derived-view"
      "FleetState is an abstract scheduler view; there is no persisted FleetState aggregate.") "fleet-slot-accounting" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.command-policy.host-execution-assumptions"
      "External binaries obey declared effects; the host kernel enforces the configured sandbox.") "command-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.trigger.dispatch-source-delivery"
      "Database change notifications and host schedule ticks may be delayed or lost; eventual rescans depend on the host running them.") "triggers" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.persistence.abstract-lifecycle"
      "The database has no per-token PersistenceState document. Local commit acknowledgements are supplied by the store adapter.") "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.storage.observation-daemon-visible"
      "Store acknowledgements and read visibility are external observations; their model does not verify DefraDB internals.") "storage-observation" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.storage.minimum-visibility-path"
      "After a successful mutation acknowledgement, the database eventually supplies read-your-writes or a later visible observation; stale reads and missing events may intervene.") "storage-observation" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.backend-health.admission-freshness"
      "A successful models-endpoint probe does not establish all provider operations, remote reachability or freshness of every replica.") "backend-health" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.event-delivery.fair-substrate"
      "The database and host scheduler eventually permit rescans and handling. Finite constructed fair traces do not prove substrate scheduling fairness.") "event-delivery" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.compaction.unique-call-ids-checked"
      "The native-message and provider transport codecs must preserve the content-bearing row projection and distinguish physical tool documents from provider-native call metadata.") "compaction" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.model.nat-typed-ids-time"
      "Nat identity and time abstractions assume faithful native identifiers and ordering; they do not establish UUID uniqueness, parse correctness or cross-node clock agreement.") "request-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.p2p-backpressure.obligation-model"
      "The bounded one-wave model does not verify the upstream database multi-wave queue, durable retry ladder, store-before-ack or restart mechanics.") "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.rendered-capture.assembled-request-artifact"
      "Transport capture parses and canonicalizes the final HTTP JSON body. Canonical JSON equality does not retain original byte ordering or whitespace.") "rendered-capture" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.rendered-capture.key-encoding-injectivity"
      "The native durable capture-key encoder must distinguish every modeled tuple; tuple equality alone does not verify an external string codec.") "rendered-capture" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.plugin-job.guest-determinism"
      "The WASM guest supplies its step function, output schema and resume bookkeeping; core proofs do not establish guest determinism.") "tool-call" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.plugin-job.single-step-driver"
      "One active runtime per node is an operating premise. Host-local job control and in-memory step coalescing do not enforce that premise across processes.") "tool-call" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "premise" "boundary.plugin-data.transaction"
      "DefraDB provides transaction serializability and its unique index as a backstop for read-then-insert plugin data writes.") "tool-policy" [Surface.runtimeInternal]
  ]

def coverageLedger : List CoverageEntry :=
  vocabularyCoverage ++ stateMachineCoverage ++ caseCoverage ++ followUpHookCoverage ++ externalPremiseCoverage ++
    [tagged (consumerCoverage "family" "event_delivery_cooldown_pruning")
      "event-delivery" [Surface.runtimeInternal]]

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
  let hasConsumer := entry.consumed
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
    ++ "\"consumed\":" ++ (if entry.consumed then "true" else "false") ++ ","
    ++ "\"accepted_boundary\":" ++ jsonString entry.acceptedBoundary ++ ","
    ++ "\"accepted_follow_up\":" ++ jsonString entry.acceptedFollowUp ++ ","
    ++ "\"feature\":" ++ jsonString entry.feature ++ ","
    ++ "\"surfaces\":" ++ surfacesJson entry.surfaces
    ++ "}"

def coverageLedgerJson : String :=
  jsonArray (coverageLedger.map CoverageEntry.toJson)

theorem coverage_uses_declared_features : coverageLedger.all (fun entry =>
    featureSurfaceRequirements.any (fun requirement => requirement.feature == entry.feature))
    = true := by native_decide

theorem coverage_entries_have_evidence :
    coverageLedger.all (fun entry => entry.consumed ||
      !entry.acceptedBoundary.isEmpty || !entry.acceptedFollowUp.isEmpty) = true := by native_decide

end Conformance.Contracts
