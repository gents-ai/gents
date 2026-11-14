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
    , required := [Surface.runtimeInternal, Surface.operatorUi]
    , deferred :=
        [ (Surface.operatorCli,
           "No CLI test replays a generated family: the runtime transition is replayed through contracts_mcp_health_transition and the view through contracts_mcp_health_view. A CLI consumer family is follow-up conversion work.") ]
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
      "family"
      "request_state_vocabulary")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "execution_origin_vocabulary")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "process_state_vocabulary")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "persistence_state_failure_policy_vocabulary")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "reconcile_phase_vocabulary")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "inference_call_state_vocabulary")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "inference_call_terminal_reason_vocabulary")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "tool_execution_tool_call_state_vocabulary")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_execution_cancel_cause_vocabulary")
      "interrupt-and-cancel" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "managed_exec_state_vocabulary")
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
      "family"
      "goals_status_vocabulary")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_status_vocabulary")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "family"
      "mailbox_kind_vocabulary")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "family"
      "mailbox_handling_vocabulary")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "family"
      "mailbox_source_kind_vocabulary")
      "mailbox" allSurfaces
  , tagged (consumerCoverage
      "family"
      "eval_outcome_kind_vocabulary")
      "eval" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "eval_provider_reason_vocabulary")
      "eval" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "eval_evidence_class_vocabulary")
      "eval" [Surface.operatorCli]
  ]

def stateMachineCoverage : List CoverageEntry :=
  [ tagged (followUpCoverage
      "state_machine"
      "Request"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached. Its terminal partition is replayed through request_state_terminal.")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "state_machine"
      "Process"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached.")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "Persistence.failClosed"
      "The database has no per-token persistence document; fail-closed and fail-open hook decisions are replayed through hook_persistence_failure.")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "state_machine"
      "Persistence.failOpen"
      "The database has no per-token persistence document; fail-closed and fail-open hook decisions are replayed through hook_persistence_failure.")
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
  , tagged (followUpCoverage
      "state_machine"
      "RuntimeReconcile"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "state_machine"
      "SessionRecovery"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached.")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "state_machine"
      "InferenceCall"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached.")
      "inference-call" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "state_machine"
      "ToolCall"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached. Its terminal partition is replayed through tool_execution_tool_call_state_terminal.")
      "tool-call" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "state_machine"
      "ManagedExec"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached.")
      "managed-exec" [Surface.agentFacing]
  , tagged (followUpCoverage
      "state_machine"
      "Goal"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached.")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "state_machine"
      "Mailbox"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached. Its terminal partition is replayed through mailbox_status_terminal.")
      "mailbox" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "state_machines_catalog_unreached"
      "model-production-gap: production has no transition function over the state-machine catalogs; each owner writes its transitions inline, so only the terminal partitions are replayed.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "request_state_terminal")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "tool_execution_tool_call_state_terminal")
      "tool-call" [Surface.agentFacing, Surface.runtimeInternal]
  ]

def caseCoverage : List CoverageEntry :=
  [ tagged (consumerCoverage "family" "client_agent_journal_recovery")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "network_protocol_resource")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "identity_secret_publication_creation")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "eval_records_published")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "eval_records_published_unreached"
      "model-production-gap: no production path lets the Node parent relationship fail or the commit fail after an eval record create is staged.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "optimization_promotion_held_transition")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "optimization_promotion_held_transition_unreached"
      "model-production-gap: no production path withholds target update permission from a caller that may update the job, forces a stale journal compare-and-set, or fails the commit after both writes are staged.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "remote_delivery_recovery_unreached"
      "model-production-gap: remote Agent delivery is refused before any send until the authenticated HTTP delivery path exists, so no production tool call reserves a signed remote intent, records an attempt or retains a target receipt.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "remote_delivery_response_unreached"
      "model-production-gap: remote Agent delivery is refused before any send, so production never verifies a target-signed terminal response against a retained receipt.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "scope_collection_rules_template_rules")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hooks_contracts_owed_trace"
      "The production record (hooks::owed::Record, owe_runner, publish, redrive, performs) is driven by every case. Its publication through the store's post-commit runtime write and the redrive's observation of core rows as the owning Agent are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hooks_contracts_owed_derive"
      "Production derive_halts decides every case. The halt derivation's observation of core rows as the owning Agent is not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hooks_contracts_owed_prune"
      "Production prunable decides every record. The deletion of pruned rows is not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "cross_actor_authorization_request_signed_targets")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "cross_actor_authorization_goal_command_signed_targets")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "cross_actor_authorization_signed_client_claim")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "cross_actor_authorization_unreached"
      "model-production-gap: the served claim fixture cannot present an unsupported or disabled native check, a transient or hidden observation, a foreign or missing native owner or authenticated author, a mismatched request document, physical target or executor, or a permission change or commit failure between the claim transaction's check and its commit.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family" "goal_links_user_owned"
      "A detached relationship operation already in flight is not fenced by a concurrent revocation.")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_agent_owned")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_foreign")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_session_goal")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_client_session_goal")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_seeded_goal")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_goal_backed_submission")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_task_fire")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_scheduled_task_fire")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_workspace_task_fire")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_agent_target")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "goal_links_closeout_goal")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "identity_publication_parent")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "callback_parent_actor")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "goal_client_delivery_contracts_trace")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage "family" "goal_client_delivery_contracts_durable_trace")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage "family" "goal_client_delivery_contracts_reopened_acknowledgement")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage "family" "goal_client_delivery_contracts_racing_proposals")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "node_provisioning_runtime_nac")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family" "node_provisioning_runtime_nac_unreached"
      "model-production-gap: every production node open enables NAC, so no production path observes a node whose NAC was never configured (not_configured).")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "root_admission_contracts_admission")
      "apply-reconcile" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_pairing_reconcile_projection"
      "The consumer exercises the production resource diff over emitted multi-resource snapshots. Connected flags are observations; actual transport dial/failure and applying operations to reach the post-state still require the transport reconciliation owner.")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "request_transition_classification")
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
  , tagged (boundaryCoverage
      "family"
      "request_execution_lease_contracts_lease_trace_unreached"
      "model-production-gap: native lease owners take one database-backed action at a time under the wall clock; no production path replays a multi-action renewal, expiry, recovery or stale-owner trace over a logical clock. No scheduler fairness or wall-clock continuity is proved.")
      "request-execution-lease" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_fire_admission")
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
      "family"
      "trigger_delivery_queue_claims")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_queue_paged_claims")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_queue_dispatch_claims")
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
      "family"
      "trigger_delivery_arrival_cursor")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family" "trigger_delivery_arrival_cursor_pending"
      "model-production-gap: production returns no list of the arrivals a consumer still owes; the durable event-source scan decides each arrival's delivery inside its loop.")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_callback_cursor")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_outcome_reasons")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_delivery_outcome_recovery")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "trigger_contracts_engine_dispatch"
      "Dispatch decisions exercise the production engine. Existing materializer tests cover correlation scope, retry exclusion and live-execution supersession. Cross-node isolation and expired-claim deadline/grace cases still need direct materializer observations. A booted schedule-kind error-writeback observation is also outstanding.")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "trigger_contracts_manual_run")
      "triggers" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "trigger_contracts_recent_run")
      "triggers" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_decision")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_readiness_gate")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_readiness_gate_served")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_claimed_readiness_claimed")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_transition")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_create")
      "durable-goals" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_create_committed")
      "durable-goals" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_capability_resolution")
      "durable-goals" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_task_goal_declaration")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_task_goal_publication_identity")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_task_goal_publication_materialized"
      "The production materializer publishes only the positive-budget scheduled goal Task; ordinary and invalid publication modes are checked through the declaration validator, not through a served publication.")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_task_goal_recovery")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_backed_runtime")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_submission")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_submission_published")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_continuation_materialization")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_file_inventory_closure"
      "The native consumer checks closure construction against modeled rows, but supplies access and denial observations as fixture input. It does not verify live ACP decisions.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "enrollment_core_trace")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "enrollment_auto_renewal_offer_unreached"
      "model-production-gap: issue_renewal_offer is reached only through a served two-node offer exchange holding one enabled current generation; conflicted, revoked or expired generations, transport mismatch and re-signed intents with another scope or window are not built per case. enrollment_auto_renewal_intent_projection replays their generation and window checks.")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "enrollment_core_frame")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "enrollment_core_digest")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "event_group_contracts_eligibility"
      "Exercises the production count/timeout eligibility owner for trigger and callback cases. Typed consumer/config/owner identity, durable marker suppression and atomic materialization require the migrated event-group owner; the old trigger-only spy cannot establish these guarantees.")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "enrollment_core_durable_projection")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "enrollment_pending_requests_satisfaction")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "enrollment_pending_requests_visibility")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "enrollment_pending_requests_listing")
      "authenticated-enrollment" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_sync_health_cases_projection")
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
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_finality_transaction")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_finality_staged_plugin")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "contracts_pack_installation_finality_unreached"
      "model-production-gap: the shared transaction owner folds one write's attempts into a sticky unconfirmed flag inside its retry loop; no scenario drives several commit attempts of one transaction, or an error with no failed attempt.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_pack_installation_hosted_evidence"
      "Binds client signer, exact create and update graphs, and relationship scope to an embedded transaction. Full plan validation, deletion and native denial remain to be exercised.")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_wire_lock_resolution")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_wire_lock_install_during_wire")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_wire_lock_wire_during_removal")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_hosted_plugin")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "contracts_pack_installation_hosted_plugin_unreached"
      "model-production-gap: production has no single decision over the reviewed digest, the client plan and native authorization, and no production scenario installs a hosted plugin root with one of them missing.")
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
      "family"
      "contracts_agent_steer_disposition"
      "Binds refusal of incompatible contexts; signed cross-requester and workspace integration remain to be exercised.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_agent_steer_target")
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
      "family"
      "contracts_agent_steer_reachability"
      "Binds admission against the ordered boundary owner, including compatible earlier steers.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_agent_delivery"
      "Binds delivery resolution; transaction admission and owned-loop delivery need native flow coverage.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_interrupt_delivery"
      "Binds the physical target selected in the message publication transaction; cross-node replication remains a separate boundary.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_agent_folded_binding"
      "Binds requester and physical-head outcome checks; cross-node replication races remain to be exercised.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "queued_steering_agent_message_outcome"
      "Binds outcome selection; physical-head binding and notification replay need native flow coverage.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "contracts_queued_steering_guard_unreached"
      "model-production-gap: wrong-head claim, incoherent supplied owner and interrupted publication are rejected by the native queue and owned-execution owners only on persisted rows no production scenario builds; the exact physical interrupt latch, like native interrupt_request_by_doc_id, permits terminal rows and preserves an existing stamp.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "aggregate_budget_ledger"
      "Exercises charged-usage summation and the owned loop budget ledger. Database selection of restart rows and missing-usage rejection still need completion-owner observations.")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (followUpCoverage
      "family"
      "aggregate_budget_ledger_unreached"
      "model-production-gap: the owned loop ledger receives usage rows already scoped to the physical request; a history that mixes requests reaches only the served rehydration test.")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "aggregate_budget_shared")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "task_hooks_contracts_admission")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "task_hooks_contracts_effective_timeout")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "task_hooks_contracts_run_admitted")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "task_hooks_contracts_run_trace"
      "Every generated trace runs through the production orchestration and managed host execution with real commands: each scripted result is produced by a real process (an exit status, a nonexistent executable, a command outliving its model-resolved timeout, or a held command cancelled through the interrupt or shutdown source its phase uses). The processes' own invocation log, with the owned work's entry, is compared against the emitted launchedTrace; attempts, outcomes and the terminal request state are compared too. The daemon placement of the before gate at claim and the after phases ahead of terminalization is fenced by daemon tests, not these traces.")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "task_hooks_contracts_daemon_revocation")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "task_hooks_contracts_recovery")
      "task-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_request_progress")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "request_admission_projected")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "request_admission_absent_requester")
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
  , tagged (consumerWithFollowUp
      "family"
      "contracts_title_admission_fresh_verifier"
      "The fresh verifier replays signed title and access-review admission through native signing and the pending-disposition owner. Watcher admission, pending rejection and crash rediscovery owners are not bound.")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family" "contracts_title_admission_fresh_verifier_unreached"
      "model-production-gap: the native fresh-verifier fixture signs the parent and title request through native signing and admission records and cannot represent these eight modeled inputs.")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_title_purpose_wire")
      "authenticated-enrollment" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_goal_decision")
      "durable-goals" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_runtime_reconcile_status")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_runtime_reconcile_router")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_runtime_reconcile_supervisor")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "contracts_runtime_reconcile_unreached"
      "model-production-gap: no production scenario drives a request finish releasing its generation, a replayed accept, retiring an unobserved generation, a resolve over a snapshot with a missing dependency, an unknown or demoted explicit selection, or an explicit request before the router observes the published generation; the served runtime fixtures cannot seed these pre-states.")
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
      "family"
      "contracts_pack_installation_eval_subject")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_identity_plan")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_reference_availability")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_reference_removal")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_pack_installation_store_record_retention")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_snapshot_demotion")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_startup_readiness")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_readiness_publication"
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
  , tagged (consumerCoverage "family" "eval_classification")
      "eval" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "eval_subject_causability")
      "eval" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "eval_document_capture")
      "eval" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "optimization_alpha_effective")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "optimization_gate")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "optimization_cost")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "optimization_decision")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "optimization_seed_coverage")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "optimization_seed_coverage_refused")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "optimization_seed_rendering")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage "family" "optimization_seed_rendering_candidate")
      "optimization" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "contracts_tool_policy_composition")
      "tool-policy" [Surface.operatorUi, Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_application_write")
      "tool-policy" [Surface.agentFacing, Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "contracts_field_read")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_write_input")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_write_input_query_filter")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_write_input_plugin_fill")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_invocation_correlation")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_lsp_action_authorization")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_field_table")
      "self-config" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_self_config_case"
      "Covers production patch admissibility and accepted merges, replays every Tools row through the always-on operator-grant guard (guard_tools_keep_grants) and every Context and Agent row, clone rows included, through reselection_keeps_grants, and replays guarded rows through the production Tools/Agent no-lockout guards, the Backend auth fence and the Profile account choice fence. Reference validation and unchanged stored state after rejection require the shared configuration transaction owner.")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_scope")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_resource_read_exposure")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_agent_decision")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_agent_materialization")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_sibling_tools")
      "self-config" [Surface.agentFacing]
  , tagged (followUpCoverage
      "family"
      "contract_cases_session_recovery_unreached"
      "model-production-gap: the model reissues over a session whose requests each carry an admission lease and a deadline clock; production reissue publishes a successor from persisted rows on a served node and records no per-request admission. The eligibility decision is replayed by session_operations_reissue_decision.")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "conformance_contract_cases_recovery_action_vocabulary"
      "model-production-gap: the action vocabulary of contract_cases_session_recovery_unreached, which has no production path.")
      "session-recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "inference_slot_accounting_reconstruction")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_notification_decide_write")
      "mailbox" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "mailbox_notification_contracts_stamp")
      "mailbox" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_sweep_contracts_progress")
      "mailbox" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "mailbox_reply_contracts_claim"
      "Decision cases cover reply matching and terminal-state eligibility. Signed-row loading, atomic request/item claim, and concurrent dismissal require transaction-owner integration tests.")
      "mailbox" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_question_contracts_questions")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_question_contracts_waits")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_question_contracts_awaiting")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_question_contracts_presentations")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_question_contracts_receipt_versions")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "mailbox_question_contracts_runtimes")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "mailbox_handoff_contracts_linked"
      "The fresh-linked case drives the stamped mailbox write, signed producer terminal owner, and linked reply admission in native storage; this consumer does not exercise AgentToolCall result publication, the owned completion loop, or atomic crash recovery across the separate mailbox and request transactions.")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family" "mailbox_handoff_contracts_linked_unreached"
      "model-production-gap: the stamped mailbox write takes the question's request, session and owner from the filing request, so production cannot file a mismatched question, and no scenario replays the reply-side mismatches through the signed reply claim after a linked handoff.")
      "mailbox" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "admission_handoff_drive_admissions")
      "tool-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "admission_handoff_owned_loop")
      "tool-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "admission_handoff_hook_effect")
      "tool-call" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family" "admission_handoff_unreached"
      "model-production-gap: production's typed oneshot admission handoff has no dispatch linearization, lease or generation fence, duplicate or misaddressed reply, unpublished header, invocation without a reply or effect observed after a stop to stage, so no production scenario replays these cases.")
      "tool-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "count_carrier_value")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "count_carrier_admission")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "count_carrier_storage")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "count_carrier_obligation_field")
      "completion-retry" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "count_carrier_event_source_field")
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
      "Proofs.Conformance.FleetSlotAccounting"
      "FleetSlotAccounting"
      "The database persists InferenceCall rows, not a FleetState aggregate; the scheduler reconstruction assumes those rows can be observed.")
      "fleet-slot-accounting" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "fleet_slot_accounting_contribution")
      "fleet-slot-accounting" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "fleet_slot_accounting_bound")
      "fleet-slot-accounting" [Surface.api]
  , tagged (consumerCoverage
      "family"
      "hook_persistence_failure")
      "persistence-failure-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "hook_persistence_mutation")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "native_completion_trace_unreached"
      "model-production-gap: no served scenario blocks a transaction statement, observes the rollback owner's return and then retries, so the timeout, cancellation, drain-then-rollback and concurrent-drain traces reach no production path.")
      "storage-observation" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "hook_persistence_visibility_unreached"
      "model-production-gap: the hook observes only its own mutation acknowledgements; read-your-writes, stale reads, stale events and late event delivery are DefraDB observations no production path classifies.")
      "storage-observation" [Surface.runtimeInternal]
  , tagged ({ consumerCoverage
      "family"
      "backend_health_admission_admission" with
      acceptedBoundary := "A provider models-endpoint probe does not establish completion health, remote-node reachability or fleet-wide document freshness." })
      "backend-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "backend_health_admission_display")
      "backend-health" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "contracts_backend_health_transition")
      "backend-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "backend_probe_schedule_select")
      "backend-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "backend_probe_schedule_field_edit")
      "backend-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "inference_registry_admission")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "title_usage_catalog")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "title_usage_budget")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "title_usage_goal")
      "inference-call" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family" "title_usage_catalog_unreached"
      "model-production-gap: purpose and binding admission (missing purpose or binding, unauthenticated or cross-purpose bindings, foreign parent scope, mismatched call key) happens in verify_historical_auxiliary_receipt over signed AgentRequest rows on a served node; the pure catalog projections never see these inputs.")
      "inference-call" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family" "title_usage_budget_unreached"
      "model-production-gap: purpose and binding admission (missing purpose or binding, unauthenticated or cross-purpose bindings, foreign parent scope, mismatched call key) happens in verify_historical_auxiliary_receipt over signed AgentRequest rows on a served node; the pure catalog projections never see these inputs.")
      "inference-call" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family" "title_usage_goal_unreached"
      "model-production-gap: the goal usage query never loads title-audit rows while projectGoal validates the whole catalog, so a conflict confined to title-audit rows (conflicting_call_id_fails_closed) cannot reach production's goal charge; purpose and binding admission (missing purpose or binding, unauthenticated or cross-purpose bindings, foreign parent scope, mismatched call key) happens in verify_historical_auxiliary_receipt over signed AgentRequest rows on a served node; the pure catalog projections never see these inputs.")
      "inference-call" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_backend_health_transition")
      "backend-health" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "contracts_native_filesystem_boundary")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_managed_exec_liveness_exec")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_managed_exec_liveness_tool")
      "managed-exec" [Surface.agentFacing]
  , tagged (followUpCoverage
      "managed_exec_tool_boundary_cases"
      "ManagedExecToolBoundaryCases"
      "Drive each generated native-tool route through its actual managed process boundary. Names and processTree flags are not evidence of runtime routing.")
      "managed-exec" [Surface.agentFacing]
  , tagged (consumerCoverage "family" "pairing_reconcile_supervisor_shutdown_race")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "pairing_reconcile_supervisor_sweep_retry")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "pairing_sweep_scheduling_sweep")
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
      "family"
      "inference_observation_trace")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "inference_observation_source_visibility")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "inference_observation_activity_route")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "Proofs.Conformance.ClientOptimisticSend"
      "ClientOptimisticSendCases"
      "The desktop optimistic-send test replays the views and traces from Proofs/Conformance/ClientOptimisticSend.lean except the runtime turn count; the native idempotent-send replay must be rebuilt against the shared request handler.")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "client_telemetry_trace")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_settings_revisions")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_settings_fields")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_data_publication_publication")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_data_publication_node_publication")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_data_publication_recovery")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_data_publication_notice_delivery")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "request_patch_publication_cases_publications")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "request_patch_publication_cases_reuse")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "request_patch_publication_cases_fresh_read")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_session_delivery_direct_read")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "storage_failure_contracts_classification")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "storage_failure_contracts_readiness")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "storage_failure_contracts_ambiguous_write")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "storage_failure_contracts_full_transaction")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "storage_failure_contracts_host_error")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_request_closure_closed")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_request_cache_identity_merge")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_shell_contracts_frontend")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_shell_contracts_desktop")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_shell_contracts_session_turn_cases_selection")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_shell_contracts_session_turn_cases_read")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_shell_contracts_session_turn_cases_projection")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "session_queue_order_arrival")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "family"
      "client_agent_readiness_projection"
      "Pending: L2 renames the consumer test and L3 the production projector it drives.")
      "runtime-reconcile" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "contract_cases_live_overlay")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_live_delta_contracts_cursor")
      "client-shell" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "client_shell_contracts_desktop")
      "request-lifecycle" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "contracts_tool_preflight")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_tool_preflight_discover")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_tool_retry_list_tools")
      "tool-call" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_tool_retry_call")
      "tool-call" [Surface.agentFacing]
  , tagged (followUpCoverage
      "family"
      "contracts_tool_retry_unreached"
      "model-production-gap: production has no retry classifier; the MCP pool retries a list_tools transport failure and never retries call_tool, and carries no idempotency evidence, so the other operations and failure classes have no production path.")
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
      "family"
      "contracts_command_policy_validation")
      "command-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_command_sandbox_selection")
      "command-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_command_env_filtering")
      "command-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "command_policy_effective_network_mode")
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "command_isolation_seatbelt")
      "command-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_command_policy_validation")
      "command-policy" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "contracts_queue_deadline_admission")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_queue_deadline_claim_deadline")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_queue_deadline_completion_delivery")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_queue_deadline_interrupt_scope")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "interrupt_queue_contracts_sequential"
      "Sequential cases bind Local and HTTP latch/drain, scheduled-origin eligibility, exact pending membership, notification bindings and replay. Native publisher timestamps do not bind modeled equal-time queue ordering. Background-process survival is a separate obligation.")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_control_contracts_interrupt_idle")
      "session-control" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_control_contracts_stop_drains")
      "session-control" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_control_contracts_resume_terminal_goal")
      "session-control" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_control_contracts_resume_after_settlement")
      "session-control" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_control_contracts_resume_waits_for_live_worker")
      "session-control" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_control_contracts_resume_waits_for_process_record")
      "session-control" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hooks_contracts_effect_commit"
      "Production validate decides every case; application runs through the conformance test's in-memory Host, whose all-or-none apply mirrors Hooks.applyEffects. The DefraDB transaction host (request::hooks::native) is crate-private and is not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "hooks_contracts_refusable_staging")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "hooks_contracts_must_commit_staging")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "hooks_contracts_must_commit_staging_unreached"
      "model-production-gap: production stages a must-commit point only after core's transition committed, so a refused transition never reaches stage_dropping; the model finds that such a case commits nothing and owes nothing.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hooks_contracts_rerun"
      "Production TransactionFailure::rerun_requested classifies a ConfigClientError::rerun carried through production wrappers (anyhow context, HookError::Operation, CommitError::Other re-erased as on the plugin-job path, text rendering). The transaction owner's rerun loop itself is not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hooks_contracts_goal_resume"
      "Production RestageOnResume and BindResumedRoot decide every case on rows built from it; the Goal row is advanced by applying their writes. Core's request.create operation and the DefraDB host are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "hooks_contracts_verdict_combination")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "hooks_contracts_claim_gate")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hooks_contracts_queue_partition"
      "Production gate runs every case and production partition combines the answers through the conformance test's in-memory Host. The DefraDB transaction host, its revision read and the Goals listener (goal::lifecycle_hooks::AssignmentScope) are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "hooks_contracts_delivery_step")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "hooks_contracts_prepared_admission")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hooks_contracts_grant_validity"
      "Production validate decides every case whose grants ride on creates. The DefraDB host's relationship publication and the Goals listener (goal::lifecycle_hooks::TaskGoalAssignment) are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "hooks_contracts_grant_validity_unreached"
      "model-production-gap: a native grant is a field of the create that carries it, so a grant on a document the result does not create has no production form; the model finds every such case invalid.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hook_decisions_supersede"
      "Production Selector::stale classifies and renders every case from row facts. The facts' extraction from AgentRequest rows (receipt verification, the parent read by document) and the retirement through the pending-supersede path are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hook_decisions_goal_halt"
      "Production halt_verdict, with its RFC 3339 parsing, decides every case. The Goal write and outcome settlement PauseOnHalt stages for a pause verdict are not driven by these cases.")
      "lifecycle-hooks" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "hook_decisions_history"
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
      "family"
      "fold_queue_contracts_input_edit_client_observation")
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
      "family"
      "fold_queue_contracts_retry_selection")
      "request-lifecycle" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "fold_queue_contracts_turn_input")
      "request-lifecycle" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "family"
      "interrupt_queue_contracts_overlap"
      "Two observed-identity overlap scripts bind real HTTP transactions against embedded completion publication, including permitted pre-latch phantom survival and post-latch replay. These schedules do not prove arbitrary distributed serialization; exact native queue ordering remains unbound.")
      "request-lifecycle" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "recovery_sweep_cases"
      "RecoverySweepCases")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_message_kill_action")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_message_kill_drive")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "session_message_kill_observed")
      "recovery" [Surface.runtimeInternal]
  , tagged { boundaryCoverage
      "family"
      "contracts_artifact_admission"
      "External binaries must obey their declared effects and the host kernel must enforce the selected sandbox." with consumed := true }
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged { boundaryCoverage
      "family"
      "contracts_artifact_spawn_launch"
      "External binaries must obey their declared effects and the host kernel must enforce the selected sandbox." with consumed := true }
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged { boundaryCoverage
      "family"
      "contracts_artifact_spawn_lsp"
      "External binaries must obey their declared effects and the host kernel must enforce the selected sandbox." with consumed := true }
      "command-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "operator_base_freeze_contracts_freeze")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_path_capability_contracts_provision")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_path_capability_contracts_sealing")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_path_capability_contracts_tree_delta")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_path_capability_contracts_integrate")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_path_capability_contracts_replay")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "client_agent_journal_private_access")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_and_file_requests_liveness"
      "Checks native filtered absence before exclusive release; complete workspace relationship publication remains separate.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_and_file_requests_agent_selection"
      "Checks explicit workflow selection and action-plan replay; native workspace relationship publication remains separate.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_capability_child_access"
      "Canonical publication attaches the shared parent atomically; ACP1 permits raw admitted writers to create orphan rows.")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "workspace_and_file_requests_quickstart_selection")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "workspace_callback_selection_visibility")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "owner_staged_targets")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "owner_staged_agent_rows")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "owner_staged_pairing")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "owner_staged_route_commands")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "owner_staged_http_apply")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "owner_staged_host_access")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "pack_placement_readers_readers")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "agent_target_access_access")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "native_route_delivery")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_datastore_write_validation")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "workspace_datastore_write_tool_call")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_root_access_root_access"
      "User publication fences on native Node update and commits the physical parent; raw direct creates remain outside the writer-fencing premise.")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerCoverage
      "family"
      "workspace_root_access_unrelated_agent_read")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_capability_child_revocation"
      "Documents the completeness limit after explicitly granting a child owner native relationship deletion.")
      "identity-permission" [Surface.runtimeInternal, Surface.api]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_capability_admission"
      "Exercises competing canonical exclusive admissions; native conflict fencing needs central validation.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_capability_cleanup"
      "Cleanup claims the parent with native update permission after checking the canonical binding inventory.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_capability_publication"
      "Checks duplicate publication after revocation and a stale retry after cleanup has begun.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_and_file_requests_quickstart_repository"
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
  , tagged (boundaryCoverage
      "family"
      "identity_operations_desired_publisher_unreached"
      "model-production-gap: a store holds only its own Node's signer, so no production path writes a desired route as a foreign actor whose signer is held.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "identity_operations_registry_writer_unreached"
      "model-production-gap: the replicated registry scenario creates no registry row naming another Node, so no production path observes its writer.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "identity_operations_reader_policy_unreached"
      "model-production-gap: the applied-pairing scenario observes only the reads, the reader Agent's update and the reader management replayed by identity_operations_reader_policy_read, identity_operations_reader_policy_update and identity_operations_reader_policy_manage; the initial route access of the runtime and user and every other route update decision are not observed.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "identity_operations_deferred_unreached"
      "model-production-gap: the served scenarios observe only the parent link or grant the identity_operations_deferred_* families replay; no production path builds a foreign embedded runtime, an unregistered Node or a failed commit, or observes the recorded parent Node.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "identity_operations_readiness_recovery_unreached"
      "model-production-gap: production cannot inject a failed parent grant or a failed publication mark, and a readiness row natively owned by an empty runtime DID cannot be written.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "identity_operations_readiness_observation_unreached"
      "model-production-gap: the recovery scenario builds at most one Node and exactly one readiness row, records a parent on every row that has a Node, and observes as the embedded runtime, so no production path observes a foreign requested runtime, a duplicated Node, a missing or duplicated readiness row, or a row with no recorded parent beside its Node.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "identity_operations_pairing_reader_unreached"
      "model-production-gap: production records no owner-authorized reader generation, so no production path observes the last granted generation, an entitlement withdrawn without a revocation, or a new or unauthorized generation.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "identity_operations_enrollment_policy_unreached"
      "model-production-gap: the thin-client scenario builds only the request a client writes before it has a Node; no production scenario builds the admin's received request, its parent, the revoked admin reader or the terminal decision.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (boundaryCoverage
      "family"
      "identity_operations_replica_grant_unreached"
      "model-production-gap: no production scenario observes the received request before any grant or between the parent's revocation and its repair; the user's write is attempted only on the linked request, the application session exists only after the repair, and the pending relation is observed only by the runtime projector, which repairs no revoked parent.")
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
      "family"
      "session_input_edit_claim")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "watcher_actors_scanned_unreached"
      "model-production-gap: the served runtime fixture provisions one Agent, so the mixed candidate set (an available, an unavailable, an unresolved and an unconfigured Agent in one view) is not built by any production test yet.")
      "event-delivery" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "node_provisioning_agent_creator_unreached"
      "model-production-gap: no served creation scenario denies the native Agent create or its parent relation to an otherwise authorized creator yet (native-create-denied, native-relation-denied).")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "delegated_finality_served_unreached"
      "model-production-gap: the session-message commit keeps no caller-side knowledge state for a pending read, a foreign receipt or an invalid abort proof, and no attempt counter or retry and failure-report permission, so pending-is-unconfirmed, foreign-receipt, no-proof-of-abort and failure-proof-cannot-undo-child have no production form.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "client_agent_journal_local_signer")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "file_review_route_contracts_subscription")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "file_review_routes")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "scope_node_route_allowed")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "scope_node_route_filter")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "scope_template")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "scope_template_filters")
      "pairing-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "file_inventory_contracts_order_window"
      "Binds canonical metadata observations and resource ordinals to native projection. Database-backed snapshot/signature binding and installed review remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "file_inventory_contracts_order_resolution"
      "Binds canonical metadata observations and resource ordinals to native projection. Database-backed snapshot/signature binding and installed review remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "file_inventory_contracts_request_coverage"
      "Binds per-request source coverage to selected physical scopes. Verified no-change producer observations and installed per-turn totals remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "file_inventory_contracts_order_comparison"
      "Binds canonical metadata observations and resource ordinals to native projection. Database-backed snapshot/signature binding and installed review remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "file_inventory_contracts_order_entry"
      "Binds canonical metadata observations and resource ordinals to native projection. Database-backed snapshot/signature binding and installed review remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "file_inventory_contracts_direct_reader_snapshot"
      "Drives actual operator reads and bounded pages under the authenticated reader. HTTP extraction and installed consumption remain separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "file_inventory_contracts_fork_snapshot"
      "Uses the native fork owner and source snapshot reader to preserve selected physical origins while excluding later parent work. Installed acceptance remains a separate obligation.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "file_inventory_contracts_shared_reader_snapshot"
      "Refines public-root visibility, exact physical binding, explicit projection read permission, revocation and reauthorization for the authenticated direct reader in one native source snapshot. Installed consumption remains a separate obligation.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "file_inventory_contracts_source_projection_pages"
      "Refines source projection scope, terminal coverage, path coherence, authenticated native owners and unsigned bounded pages. Remaining canonical admission, fork and publication refinements stay separate obligations.")
      "file-inventory" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "schema_argument_repair_tool_arguments")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "schema_argument_repair_owned_loop")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "schema_argument_repair_arguments")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "invalid_tool_progress_contracts_composed")
      "completion-retry" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "repeated_tool_failure_contracts_owned_loop")
      "completion-retry" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_tool_flag")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_job_guard")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_job_guard_native")
      "self-config" [Surface.agentFacing]
  , tagged (followUpCoverage
      "family"
      "contracts_self_config_job_guard_unreached"
      "model-production-gap: an operator Tools write bypasses the grant guard and no production scenario drives an operator write of a job block.")
      "self-config" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_policy_plugin_model_slot_allowed")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_limit_budget")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_network_admission")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_address_public")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_call_bindings")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_call_admissions")
      "tool-policy" [Surface.agentFacing]
  , tagged (followUpCoverage
      "family"
      "plugin_resources_call_admissions_unreached"
      "model-production-gap: a declaration naming write fields under read access fails validation, so no installed plugin carries it and bind admission never runs for it.")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_invocations")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_publication")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_receipt_commits")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "tool_policy_plugin_calls_child_admissible")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_effects_admissions")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_effects_terminal_presentation")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_effects_child_accounting")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_host_call_lifetimes")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_compute_allowances")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_sleeps")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_supported_routes")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_availabilities")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_spawned_dispatchers")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_reservations")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_result_sizes")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_settlements")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_consents")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_job_admission")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_source_boundary_dispatch")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_job_dispatch_refusal")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_job_adoption_step0")
      "plugin-jobs" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_adoption_host_pin")
      "plugin-jobs" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_adoption_handle")
      "plugin-jobs" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_adoption_address")
      "plugin-jobs" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_control_kill")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_job_control_resume")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_job_control_result_read")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_job_control_result_offer")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_job_control_step_failure")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_job_spawn_keys")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_provenance_read")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_install_record")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_installer_authority")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_script_origin")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_pack_block_admission")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_policy_validity")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_target_wiring")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_published_read_deferral")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_wire_sample")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_lineage_starting")
      "plugin-jobs" [Surface.runtimeInternal, Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "plugin_job_lineage_opening")
      "plugin-jobs" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "plugin_job_recoveries")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_wakes")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_restart_plugin_job")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "r6_backgrounding_cases"
      "R6BackgroundingCases")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_data_writes")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_data_queries")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_data_filters")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_data_notes")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_scratch_admissions")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_host_import")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "plugin_resources_child_routes")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_self_config_case")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_plugin_job_opening")
      "plugin-jobs" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_timeouts_foreground_pair")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_timeouts_cli_timeout")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_timeouts_background_lifetime")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_timeouts_wait_policy")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "tool_timeouts_lsp_policy")
      "tool-policy" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "workspace_path_alias_admission")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_claimed_publication_contracts_historical")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_claimed_publication_contracts_running_wait")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_claimed_publication_contracts_wait_observation")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_first_request_binding_binding")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_first_request_binding_task")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_first_request_binding_manual_task")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_first_request_binding_continuation")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_first_request_binding_edge")
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
      "family"
      "goal_request_assignment_contracts_message_input")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_delivery_input_delivery")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_delivery_input_manual_task_run")
      "durable-goals" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "goal_request_assignment_contracts_input_binding")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "goal_request_assignment_contracts_fresh_session")
      "durable-goals" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "goal_request_assignment_contracts_signed_binding")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_request_assignment_contracts_unread_history_scope")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_wake_receipt_contracts_receipt")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_wake_coalescing_contracts_assignment_wakes")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_wake_ancestry_contracts_ancestry")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_wake_ancestry_contracts_continuation_opening")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_tool_context_contracts_context")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_request_ancestry_contracts_scope")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_request_ancestry_contracts_ancestry")
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
      "family"
      "native_refinement_observation")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_job_commit_kills")
      "plugin-jobs" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "native_refinement_claim_scenario")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "native_refinement_claim_unreached"
      "model-production-gap: no production path reaches these emitted cases yet (the served claim scenario builds only the agent's own client-owned intent at its observed revision; claims as another agent, for another owner or invocation, or after a revision change are not driven)")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "native_refinement_client_process_receipt")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "native_refinement_grant_scenario")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "native_refinement_grant_unreached"
      "model-production-gap: no production path reaches these emitted cases yet (production publishes a control outcome only inside the claim's fenced transaction; unfenced publication is unreachable)")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "native_refinement_signed_control")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "native_refinement_stop_action")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "native_refinement_process_identity")
      "process-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "native_refinement_target_resolution")
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
      "family"
      "goal_client_control_contracts_control")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_client_control_contracts_denied_submission")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_client_control_contracts_foreign_submission")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_client_control_contracts_submission_trace")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "goal_client_start_cases"
      "GoalClientStartCases")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_client_mutation_contracts_local_receipt")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_observation_contracts_acceptance")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_observation_contracts_recovery")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_session_observation_contracts_classification")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_session_observation_contracts_presentation")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage
      "Proofs.Conformance.GoalSessionObservation"
      "GoalSessionFoldCases")
      "durable-goals" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "plugin_approval_unscoped_step")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_approval_scoped_step")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "plugin_approval_delivery")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_edit_contracts_objective_whitespace_runs")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_operator_resume_contracts_resume_transaction")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_operator_resume_contracts_resume_transaction")
      "durable-goals" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "goal_operator_resume_contracts_resume_on_target")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_claimed_publication_contracts_claimed_target")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_claimed_publication_contracts_identity_candidate")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_claimed_publication_contracts_identity_staged")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "goal_claimed_publication_contracts_identity_staged_unreached"
      "model-production-gap: no production scenario stages a claimed continuation from a parent request owned by another identity; the goal source never offers such a parent and the staging fixture has no foreign parent request.")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_operator_resume_contracts_publication")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_operator_resume_contracts_config")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "goal_reset_resume_contracts_reset")
      "durable-goals" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_restart_native")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_restart_session_message")
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
      "family"
      "contracts_restart_missing_parent")
      "recovery" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_restart_classification")
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
      "family"
      "composed_termination_linked"
      "Covers four persisted tool recovery/cancellation outcomes of ComposedState.linkedTermination (justified by linkedTermination_reachable_from_initial over the ToolTermination theorems) through canonical accepted admission and physical request/tool rows: ToolCallLifecycle::cancel_before_dispatch, ToolCallLifecycle::cancel_during_run and the ToolCallLifecycle::recover_all deadline sweep. Full composed request, admission and clock traces still need their runtime owners.")
      "composed-invariants" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_background_admission_bound")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_background_receipt_contention")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_tool_output_canonical_source")
      "background-tools" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "contracts_tool_output_paging")
      "background-tools" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_interrupt_disposition")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_interrupt_in_flight")
      "background-tools" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "transcript_cases_native_completion"
      "Native owners cover ordering, model-scripted parallel completion and pair closure. Full coverage needs transaction fault injection for lost-ack duplicate delivery replay and a permissive orphan-result publication owner.")
      "transcript" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "transcript_cases_native_abandon")
      "transcript" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "transcript_cases_snapshot_projection")
      "transcript" [Surface.operatorUi]
  , tagged (consumerCoverage "family" "agent_identity_permission_decide")
      "identity-permission" [Surface.api]
  , tagged (followUpCoverage "family" "agent_identity_permission_selection_unreached"
      "model-production-gap: production has no node-scoped Agent selector over Agent rows to replay; foreign-node, unknown and duplicate DIDs are rejected only by the model before /identity/decide, which receives an already selected DID.")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "identity_household_cases"
      "IdentityHouseholdCases")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "session_publication_task")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "session_publication_client_reservation")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "session_child_atomic_client_request")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "session_child_atomic_client_request_unconfirmed")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "session_child_atomic_client_task_request")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "agent_identity_derivation")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "agent_identity_collection_locality")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "agent_identity_replication_admission")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "agent_identity_desktop_subscription")
      "identity-permission" [Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "family"
      "worker_capacity_contracts_transition"
      "Bind modeled acquire, full-capacity refusal and release to the slot-owned Rust capacity scheduler. No request parks a worker while waiting on another session.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "canonical_execution_gate_cases"
      "CanonicalExecutionGateCases"
      "Generated native scripts bind transactional lease, publication, recovery and tool owners. Title scripts bind signed parent-only admission, own claim/begin, renewal, Complete/Partial/retracted audit closure, retry coordinates, terminal-parent isolation and purpose denial. Rows marked nativeGap retain explicit replication or unmaterialized-input premises; trace summaries and finite cases do not establish the application's universal invariants in Rust.")
      "canonical-output" [Surface.runtimeInternal, Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "dispatch_observation_contracts_spawned_target")
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
      "family"
      "title_text_generation")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "title_text_summary")
      "canonical-output" [Surface.operatorUi]
  , tagged (boundaryCoverage
      "family"
      "title_admission_join_unreached"
      "model-production-gap: the join of the observed request row with the own-lease title or access-review world runs only through the Lean activation and claim owner; no production scenario materializes those rows for the native watcher admission and claim owners, nor renewal, recovery or output capture.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "payload_presentation_contracts_lengths"
      "The four generated cases bind stored and presented payload-field bytes and missing-dependency failure to native canonical reconstruction; payload_presentation_contracts_malformed_lengths binds integrity rejection of a corrupted stored flush. Provider serialization, estimator output, and compaction threshold remain unbound here; the real serialized-request/threshold experiment is native.external-projected-request-threshold.")
      "canonical-output" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "payload_presentation_contracts_malformed_lengths")
      "canonical-output" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "family"
      "terminal_diagnostic_contracts_presentation_plan"
      "Eight valid generated inputs bind exact native presentation ranges, literals, and rendered bytes. Two invalid UTF-8 inputs are rejected at the adapter representation boundary via String::from_utf8; the production presentation helper accepts typed strings. This pure binding does not establish capture of uncommitted process bytes, terminal CAS authority, crash recovery, or provider continuation; those remain obligations of their existing owners.")
      "canonical-output" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "terminal_diagnostic_contracts_streamed_failure")
      "canonical-output" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "terminal_diagnostic_replay_contracts_stored_replay")
      "canonical-output" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contract_cases_compaction_projection_join_staged_turn_reduction")
      "compaction" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "contract_cases_compaction_projection_join_projection_join"
      "model-production-gap: no production path reaches these emitted cases yet. Model-derived projection/estimate/reduction and rebuilt-admission observations bind exact input identities, failure propagation and threshold/output-capacity boundaries. Provider projection and estimation are explicit fallible native premises, not modeled serializers. Native adapters and the real serialized-request experiment remain pending; controlled observations are not native consumer coverage.")
      "compaction" [Surface.agentFacing]
  , tagged (followUpCoverage
      "family"
      "compaction_canonical_projection_unreached"
      "model-production-gap: production composes canonical reconstruction, provider projection, estimation and reduction inside gents-loop request assembly over the serialized request; no production function takes a canonical world, selected identities and supplied estimates. Concrete canonical records and selected identities pin the full native messages at initial and checkpoint-rebuild callbacks and reject missing or loading dependencies without shortened history.")
      "compaction" [Surface.agentFacing]
  , tagged (followUpCoverage
      "family"
      "contract_cases_compaction_canonical_projection_repaired_admission_unreached"
      "model-production-gap: production admits a fresh provider request inside gents-loop request assembly over the serialized request, where projection and estimation are the provider's own calls; no production function takes a supplied projection outcome and estimate. This does not model the repair transformation.")
      "compaction" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "family"
      "canonical_output_projection_view"
      "project_live replays every projection observation and its whole view (published envelope and reconstructed native message, preview streams, or the loading, denied, conflicted, invalid, retracted and absent verdicts) is compared with the model's project. This binds the projection owner, not ACP or genesis validation.")
      "canonical-output" [Surface.agentFacing, Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "family"
      "canonical_output_projection_presentation"
      "The two published mixed/all-opaque reasoning cases bind present_message to renderedKinds. This binds stateless reasoning presentation, not projection selection, ACP or genesis validation.")
      "canonical-output" [Surface.agentFacing, Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "family"
      "canonical_output_projection_preview"
      "The two published mixed/all-opaque reasoning cases bind render_reasoning_text to renderedKinds. This binds the stateless per-event rendering function, not stream-event delivery or reconnect behavior.")
      "canonical-output" [Surface.agentFacing, Surface.operatorUi]
  , tagged (consumerWithFollowUp
      "family"
      "reasoning_audit_prefix"
      "Generated open, partial, retracted, continued-signature and gap observations bind exact native dense-prefix reconstruction, including subtype and bytes. They do not establish provider-event capture, durable write admission, ACP, or full replica completeness beyond the observed prefix.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "reasoning_deltas_coalesce"
      "Generated fragment cases drive the native accumulator across reasoning identities, signed boundaries, text boundaries and UTF-8 bytes.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "reasoning_audit_signature"
      "Generated typed cases bind published-header signature agreement to retained signature bytes, including provider inline-signature rejection without a retained stream and authored inline-signature allowance. This does not establish provider-event capture, durable write admission, or ACP.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family"
      "reasoning_audit_signature_unreached"
      "model-production-gap: native OutputSegment payload is a UTF-8 String, so a flush whose modeled bytes are not UTF-8 cannot be written or translated; the model rejects such a signature run as malformed.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "auxiliary_output_owned"
      "Generated compaction/fallback cases bind claimed append denial, own begin/append, Complete or Partial exact close, stale-writer close replay denial, and nonempty auxiliary publication denial to native request/output owners. Empty-assistant publication is Lean-only because the native encoder rejects that shape. This does not cover title ownership, recovery after crash, or every provider cancellation path.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "auxiliary_output_sink"
      "Generated compaction/fallback cases drive the non-Claude auxiliary sink with typed reasoning, signature, encrypted and redacted observations, then compare durable bytes, exact closure, private audit reconstruction and absent public live/message output to Lean. This does not cover crash durability before sink acknowledgement, title ownership, recovery after crash, or every provider cancellation path.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "auxiliary_output_projection"
      "Generated compaction/fallback records bind exact private audit bytes and absent public live projection to native reconstruction. The expired-lease recovery plan closes the record without retained streams. This is a read-only projection check; it does not establish provider-event capture, durable write admission, recovery transactions, title ownership, or ACP.")
      "canonical-output" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "claude_prompt_assembly_wire_start"
      "The four generated wire-start cases bind initial signature plus later delta, absent initial signature, ordered rejection of later thinking, and malformed signature type at the native SSE parser. The native test compares final content/error, not intermediate modeled steps or durable output capture.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_command_grants_answers")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_command_grants_lookup")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_command_grants_revocations")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "workspace_permissions_selection")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_permissions_persisted_selection"
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
  , tagged (consumerCoverage
      "family"
      "access_review_task_eligibility")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "access_review_selected_task_eligibility"
      "Generated Task documents bind node, task, agent, operator-selected policy text, eligibility and excluded execution effects. Store lookup, template evaluation, policy fingerprint integrity, concurrent edits and provider behavior remain native adapter boundaries.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_command_root_selection"
      "Canonical host paths drive the production writable-root selection. A macOS execution regression checks real writes inside the project, rejection in its sibling and Full access restoring the configured root; other platforms retain the existing sandbox-unavailable barrier.")
      "tool-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "operator_paths_command_guard")
      "tool-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "operator_paths_protection"
      "Generated decisions drive the shared path guard with real files, hard links, nested approval queues, missing targets and failed filesystem inspection. File-tool regressions verify that both reads and writes reject operator aliases before touching contents. Filesystem handle identity and concurrent external namespace changes remain native premises.")
      "tool-policy" [Surface.agentFacing, Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_permissions_routing"
      "Generated decisions bind workspace isolation, configured ceilings, action/revision grants and automatic-review fallback to the native routing owner. Canonical host paths, command fingerprints, sandbox enforcement and reviewer accuracy require adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_permissions_sandboxed_background"
      "Generated decisions bind workspace isolation, configured ceilings, action/revision grants and automatic-review fallback to the native routing owner. Canonical host paths, command fingerprints, sandbox enforcement and reviewer accuracy require adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "workspace_permissions_unsandboxed_background"
      "Generated decisions bind workspace isolation, configured ceilings, action/revision grants and automatic-review fallback to the native routing owner. Canonical host paths, command fingerprints, sandbox enforcement and reviewer accuracy require adapter checks.")
      "tool-policy" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_service_tier_validation"
      "Generated decisions cover every provider family with unset, Standard and Fast selections; native validation rejects unsupported carriers. Provider account/model eligibility and delivered speed are external.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_service_tier_loop_config")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_service_tier_transport")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_prompt_assembly_responses_storage"
      "The generated cases drive loop_config and the loop's own ProviderInputCounter projection, so wire params and accounting are checked together. Premise: reasoning unset and a non-empty preamble. The OAuth transports' own HTTP store patch is fenced by its unit tests, not by this case family.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_prompt_assembly_responses_effort"
      "The generated cases drive loop_config and the loop's own ProviderInputCounter projection, comparing the projected reasoning.effort. Premise: OpenAiCompatible on the Responses wire; kind and wire are not modelled, so these cases cannot catch a wrong kind/wire guard (pinned by inference_setup::tests::sent_reasoning_effort_leaves_other_kinds_and_wires_alone).")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_compaction_reducer_gate")
      "compaction" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_compaction_reducer_apply")
      "compaction" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_compaction_reducer_gate_open")
      "compaction" [Surface.agentFacing]
  , tagged (followUpCoverage
      "family"
      "contracts_compaction_reducer_apply_unreached"
      "model-production-gap: production summarizes by a token budget and no production reducer applies the model's fixed split index; the identity reducer exists only in the model. Summarize and identity message counts, pair and order preservation, identity and idempotence have no native observation.")
      "compaction" [Surface.agentFacing]
  , tagged (followUpCoverage
      "family"
      "contracts_compaction_reducer_identity_gate_unreached"
      "model-production-gap: the identity reducer exists only in the model; no production compaction path evaluates its validity gate.")
      "compaction" [Surface.agentFacing]
  , tagged (consumerWithFollowUp
      "family" "contracts_protected_replay_compaction"
      "Native replay association and reduction ceilings preserve the entire captured prefix of required reasoning. A nonempty required set refuses summarization with CannotFit; an empty set preserves the ordinary retention policy. Retirement of older requirements remains deferred. Generated cases bind modeled assistant blocks to native ceiling and split selection; the summary-provider refusal has separate native coverage.")
      "compaction" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_compaction_cursor")
      "compaction" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_routing_affinity")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_routing_affinity_dispatches")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_current_input_selection"
      "Binds canonical header classification and selection to CurrentInput. Full reconstructed-history regression additionally covers retaining a current-request tool result; arbitrary imported-seed reachability is not claimed.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "contracts_wake_input_projection")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family" "contracts_prompt_assembly_sanitize")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_prompt_assembly_layer")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "prompt_assembly_repair_args")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_prompt_assembly_budget")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_prompt_assembly_turn_budget")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "contracts_prompt_assembly_compaction_trigger")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (consumerCoverage
      "family"
      "prompt_assembly_budget_retention")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_claude_map")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "claude_prompt_assembly_body_system")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "claude_prompt_assembly_body_effort")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "claude_prompt_assembly_body_plan")
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
      "family"
      "contracts_claude_stream")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_claude_thinking_stream"
      "Drives the native Claude Messages SSE parser and compares cumulative provisional text and sealed signed/redacted content. The owned loop's persistence and provider continuation are not exercised here.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_claude_thinking_seal"
      "Drives modeled nonempty thinking-start text through the native parser and canonical accumulator, checking exact sealed content. Owned-loop persistence and provider continuation remain unbound.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family" "contracts_prompt_assembly_claude_replay"
      "Reconstructs native assistant blocks and checks exact Claude body replay or fail-closed error, including empty text and unsupported assistant image. Actual HTTP transport and request budgeting are not exercised here.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family" "contracts_prompt_assembly_claude_replay_round_trip"
      "Drives the streamed-signed-redacted-tool case through the owned loop: its canonical blocks become turn-1 SSE, the persisted message must retain their exact grouping and payloads, and the turn-2 capture must equal the modeled replay. Other replay cases and the thinking-stream family remain unbound through the owned loop.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family" "contracts_prompt_assembly_reasoning_suffix"
      "Binds native two-phase selection (provenance suffix, stage assembly, turn location and capture-derived admissible suffix) over the model's row cases and every step of the replay scenarios (compaction, repair, strip-and-retry, tool and issuer changes, interruption, restart, Claude signatures and redaction, Responses ciphertext). Native flattening of real provider bodies is bound by gents-loop replay_frontier tests, not by these cases.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family" "contracts_prompt_assembly_replay_shape"
      "Binds original-index block shaping and misaligned-sidecar rejection. Complete compaction-history carriage is exercised through native request assembly tests.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family" "contracts_prompt_assembly_replay_prefix"
      "Binds the two capture checks and their agreement with leading-reasoning removal on flat items. Provider acceptance itself is an external premise documented on ReplayFrontier.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family" "contracts_prompt_assembly_claude_checkpoint"
      "Models source-tagged selected assistant occurrences, the exact reduction split that keeps a pending tool round out of a summary, and the strict Claude codec. Replay selection is PromptAssemblyReasoningSuffixCases; this model does not prove tag issuance (#1693).")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "provider_tool_identity_normalized")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "provider_tool_identity_projected")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "provider_tool_identity_assembled")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family" "contracts_prompt_assembly_assistant_order"
      "Checks the mode-selected native assistant-content ordering helper. It does not establish that every provider dispatch selects the correct mode.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family" "contracts_prompt_assembly_mode_sanitize"
      "Checks the composed native provider-view sanitizer on finite coherent, unique-call-id witnesses, and that the tagged association view is the native-order view whose send-boundary ordering is that composed view. General native reachability and arbitrary transcript premises remain outside this fence.")
      "prompt-assembly" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_rendered_capture")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contract_cases_rendered_capture_storage_resolution"
      "The native consumer checks full and delta codec reconstruction with an injected base and witness. It does not exercise DefraDB base lookup, authorization, or provider-send ordering.")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contract_cases_durable_reduction"
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
      "family"
      "budget_rehydration_ledger"
      "Drives absent, zero and positive pinned limits with inference/compaction rows through the real physical-request query and ledger factory. Full process restart and InferenceCall write ownership remain separate boundaries.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_rendered_capture_key")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "capture_scope_parse")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_capture_order")
      "rendered-capture" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_rendered_capture")
      "rendered-capture" [Surface.operatorCli]
  , tagged (consumerCoverage
      "family"
      "contracts_rendered_capture")
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
      "family"
      "contracts_mcp_health_transition")
      "mcp-health" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_mcp_health_view")
      "mcp-health" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "tool_execution_cancel_cause_vocabulary")
      "interrupt-and-cancel" [Surface.operatorUi]
  , tagged (followUpCoverage
      "state_machine"
      "Request"
      "model-production-gap: production has no transition function over this catalog; it is emitted as the unconsumed family state_machines_catalog_unreached. Its terminal partition is replayed through request_state_terminal.")
      "interrupt-and-cancel" [Surface.operatorUi]
  , tagged (consumerCoverage
      "family"
      "contracts_workspace_quickstart_transition")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (followUpCoverage
      "family" "contracts_workspace_transition_unreached"
      "model-production-gap: production has no lifecycle-transition owner; host executor plans write each transition inline and no scenario replays these transitions through them, and the illegal transitions have no production path.")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_workspace_binding_admission")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp
      "family"
      "contracts_callback_journal"
      "Generated journal-prefix observations exercise the actual journal owner. Invocation-state legality, denied execution and result-emission ordering need the callback executor consumer; the removed Rust predicate copy did not establish them.")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_callback_retry")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_callback_recovery")
      "isolated-workspaces" [Surface.runtimeInternal]
  , tagged (consumerCoverage
      "family"
      "contracts_callback_denial")
      "isolated-workspaces" [Surface.runtimeInternal]

  , tagged (consumerCoverage "family" "contracts_runtime_reconcile_selection")
      "runtime-reconcile" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp "request_input_cases" "RequestInputCases"
      "Canonical input serde only. Signed canonical_input_fields/bytes, context whitelist, title materialization and verified goal receipt checks need the migrated admission/signing owners.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "request_input_cases" "RequestInputCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "request_input_cases" "RequestInputCases")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "session_reply_preview_observation")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "task_lineage_originating")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "task_lineage_scoped_task")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "question_reply_source")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "runtime_input_output_obligation")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerWithFollowUp "family" "contracts_pending_timeline"
      "Generated pending timeline cases bind physical identity, admission order, same or following request anchors, hidden or missing anchor fallback, tool-first replication, orphan groups and overlay ownership to the shared protocol ordering owner. Durable request lookup and authored-message suppression remain bridge-owner obligations.")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_task_opening")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_fork_reference_authority")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_tip_coverage")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_session_rename")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_tool_activity")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_atomic_admission_unattended")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "contracts_atomic_admission_unreached"
      "model-production-gap: atomic admission runs inside the served request admission transaction with DefraDB identities and ACP relationships; no production function returns the modeled admission state, and a scenario observes only the unattended creation title.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "contracts_session_advance_unreached"
      "model-production-gap: the session observation owner advances the public head inside served request admission transactions; no production function takes a modeled document and request rows, and no scenario builds these request histories.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "contracts_session_refresh_unreached"
      "model-production-gap: the session observation owner refreshes the public head inside served request transactions; no production function takes a modeled document and request rows, and no scenario builds these request histories.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "contracts_edit_anchor_fork_unreached"
      "model-production-gap: the served fork fixture publishes user-text turns only; edit anchors in histories with assistant, tool-result or empty user messages, duplicate keys or absent anchors are not built by a production scenario.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_session_selection")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_session_selection_client_store")
      "client-shell" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_fork_readers")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "contracts_fork_publication_unreached"
      "model-production-gap: production publishes a fork through its served transaction with DefraDB document identifiers; no production function returns the copied history for comparison with SessionFork.publish, and no scenario builds these source histories.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_archive_transition")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_archive_receipts")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "contracts_archive_transition_unreached"
      "model-production-gap: archive authorization is DefraDB ACP on the caller's transaction before the revision transition runs; no production scenario replays the unauthorized archive writes.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "session_documents_retry_unreached"
      "model-production-gap: the model selects the retry parent from the session document's observation and the exact physical request rows in one transaction; production retry eligibility reads persisted rows without that observation, so no production path takes these inputs. No fixture-local state machine may substitute.")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "family" "contracts_background_wake_rows_unreached"
      "model-production-gap: background publication still redrives a failed wake through its pre-migration owner; no production path publishes the successor from exact physical-parent and cross-requester authoritative rows. Existing DB wake tests do not establish these row verdicts.")
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
  , tagged (consumerCoverage "family" "configuration_scope_discovery")
      "apply-reconcile" [Surface.runtimeInternal]
  , tagged (boundaryCoverage "family" "configuration_scope_discovery_unreached"
      "model-production-gap: a backend observation carries no owner; production reaches a catalog only through the owner-qualified backend document that holds it, so no production call is handed a foreign owner's same-labelled catalog.")
      "apply-reconcile" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "initialization_initial_inference")
      "apply-reconcile" [Surface.operatorCli]
  , tagged (followUpCoverage "family" "event_delivery_group_clock_unreached"
      "model-production-gap: the durable group clock stamps first-seen and quiescence with the wall clock inside a served transaction and keeps no cached first-seen observation; no production path takes now, timeout or a cached first-seen as an input.")
      "triggers" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "event_delivery_group_capture_unreached"
      "model-production-gap: production captures a callback group only inside the served callback scan (materialize_group), which loads the group by the delivery's own key; mismatched or trigger candidates have no production path and the eligible and quiesced cases need a served callback-binding scenario.")
      "triggers" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "contracts_callback_transition_unreached"
      "model-production-gap: the callback executor's claim, run, succeed, fail and retry steps happen inside the served engine's database writes and no production function applies one step to an invocation; denial and interrupt state and journal replay through contracts_callback_denial and contracts_callback_recovery, but the invocation document stores the origin group as a derived key string and has no emission flag, so these input, origin and emission preservation witnesses have no native replay.")
      "triggers" [Surface.runtimeInternal]
  , tagged (consumerCoverage "family" "contracts_callback_saturation_trace")
      "triggers" [Surface.runtimeInternal]

  , tagged (consumerCoverage "family" "contracts_discovery_guidance_presented")
      "prompt-assembly" [Surface.agentFacing]
  , tagged (followUpCoverage "family" "node_provisioning_reservation_unreached"
      "model-production-gap: no production path reaches these emitted cases yet (file-sync-failed: no production path makes the file sync of a newly created bootstrap marker fail)")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "node_provisioning_bootstrap_grant_unreached"
      "model-production-gap: no production path reaches these emitted cases yet (not-exclusive, foreign-runtime, unauthenticated-client, existing-or-unknown-node, store-not-fresh, reservation-not-durable, nac-disabled, already-published: init decides each inline across its store lock, runtime match, client authentication, Node and store observation, reservation, NAC status and publication steps; no production function takes the bootstrap observation)")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "remote_delivery_delivery_unreached"
      "model-production-gap: no production path reaches these emitted cases yet (every case: production refuses delivery to another runtime until the authenticated HTTP delivery path exists (plan_with_delivery), so remote creation, the working-agent grant and response confirmation have no production owner)")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "remote_delivery_claim_unreached"
      "model-production-gap: no production path reaches these emitted cases yet (every case: production never creates a request from another runtime, so no claim transaction observes the remote sender's session, invoke or node permission)")
      "request-lifecycle" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "session_child_child_scope_binding_unreached"
      "model-production-gap: no production path reaches these emitted cases yet (absent_session_requester, foreign_collection, foreign_physical_session, empty_physical_session, foreign_child_agent, invalid_auxiliary_signature, invalid_auxiliary_shape, foreign_auxiliary_requester, foreign_auxiliary_node, foreign_auxiliary_session, foreign_auxiliary_agent, missing_parent, invalid_parent_signature, parent_not_normal, foreign_parent_document, foreign_parent_id, foreign_runtime_source, foreign_parent_node, foreign_parent_session, foreign_parent_agent, foreign_parent_requester, absent_parent_requester, missing_native_owner, foreign_native_owner, foreign_creator, foreign_authenticated_creator, failed_grant, failed_marker, uncommitted, completed_then_revoked: the served binding fixture writes the session, signed parent and auxiliary request through their production writers and cannot forge their signed contents, owners or collection)")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "session_child_child_scope_repair_unreached"
      "model-production-gap: no production path reaches these emitted cases yet (every case except paired_title_segment, missing_parent, invalid_parent_signature, foreign_native_owner and completed_then_revoked: the served repair fixture cannot fail a native grant or marker write, leave the child uncommitted, or forge binding fields of signed rows)")
      "identity-permission" [Surface.runtimeInternal]
  , tagged (followUpCoverage "family" "session_child_client_request_recovery_unreached"
      "model-production-gap: no production path reaches these emitted cases yet (grant_failed, wrong_signature, already_completed_revoked: the served retry fixture cannot fail the working Agent grant or marker, sign a mismatched request, or start from a completed publication whose grant was revoked)")
      "identity-permission" [Surface.runtimeInternal]
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

example : followUpHookCoverage.all (fun entry =>
    ["RuntimeReconcile", "ToolExecution", "CommandPolicy"].all fun owner =>
      (entry.domain.splitOn owner).length == 1) = true := by native_decide

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
  , tagged (boundaryCoverage "premise" "boundary.count-carrier.non-null-id"
      "DefraDB refuses a non-null ID field declaration (NonNull variant for type ID is not supported), so no count carrier of that schema is ever stored.") "completion-retry" [Surface.runtimeInternal]
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
