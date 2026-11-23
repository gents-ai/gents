import Proofs.Conformance.ContractTypes

/-!
The coverage ledger. Every family the snapshot emits is replayed by exactly one
native test, which the consumer scan in `gents-lean-vocab` finds from the
family's snapshot key; a consumed family has no entry here. The ledger lists
only what no test consumes: `boundaries`, facts about the database, operating
system, host, provider or WASM guest that no Gents test observes, each with its
reason; and `openFamilies`, emitted families that have no consumer.
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

/-- An unconsumed coverage item that stays unconsumed: a name beginning
`boundary.` is a premise about something outside Gents; any other name is an
emitted family whose cases only such a premise decides. -/
structure Boundary where
  name : String
  reason : String
  deriving Repr

def Boundary.toJson (boundary : Boundary) : String :=
  "{"
    ++ "\"name\":" ++ jsonString boundary.name ++ ","
    ++ "\"reason\":" ++ jsonString boundary.reason
    ++ "}"

def boundaries : List Boundary :=
  [ { name := "boundary.backend-health.admission-freshness"
    , reason := "A successful models-endpoint probe shows only that the endpoint answered; it does not establish that other provider operations work, that the remote stays reachable, or that every replica is fresh." }
  , { name := "boundary.command-policy.host-execution-assumptions"
    , reason := "External binaries behave as their declared effects say, and the host kernel enforces the sandbox the command policy selects." }
  , { name := "boundary.count-carrier.non-null-id"
    , reason := "DefraDB refuses a non-null ID field declaration (NonNull variant for type ID is not supported), so no count carrier of that schema is ever stored." }
  , { name := "boundary.event-delivery.fair-substrate"
    , reason := "Delivery liveness assumes the database and host scheduler eventually run every rescan and handler. Finite constructed traces cannot prove scheduler fairness." }
  , { name := "boundary.event-delivery.substrate"
    , reason := "Persisting, depersisting and enqueueing, dropping or consuming subscription events are DefraDB and transport behaviour. The executable relation checks their input witnesses, and native delivery families observe emitted requests and local suppression, not a transport queue." }
  , { name := "boundary.inference-slots.running-row-derived"
    , reason := "Held backend slots are derived from InferenceCall rows in the running state; the schema has no held-slot document, so slot accounting is checked against that derivation." }
  , { name := "boundary.model.nat-typed-ids-time"
    , reason := "Models represent identifiers and times as natural numbers and assume native identifiers are unique and parse faithfully. They establish neither UUID uniqueness nor agreement between the clocks of different nodes." }
  , { name := "boundary.p2p-backpressure.obligation-model"
    , reason := "P2PBackpressure models one bounded wave. The database's multi-wave queue, durable retry ladder, store-before-ack and restart behaviour belong to defradb.rs and are taken as given." }
  , { name := "boundary.persistence.abstract-lifecycle"
    , reason := "PersistenceState abstracts the store adapter's commit acknowledgement for a request; the database stores no per-request persistence document, so the state is observed only through that acknowledgement." }
  , { name := "boundary.plugin-data.transaction"
    , reason := "DefraDB provides serializable transactions and enforces its unique index, which plugin data writes rely on to reject a concurrent duplicate after a read-then-insert." }
  , { name := "boundary.plugin-job.guest-determinism"
    , reason := "The WASM guest supplies its step function, output schema and resume bookkeeping; core proofs take the guest's behaviour as given and do not establish its determinism." }
  , { name := "boundary.plugin-job.single-step-driver"
    , reason := "A node's store admits one runtime process at a time through an operating-system advisory lock, so one process drives a node's plugin job steps; in-process step coalescing relies on that lock holding." }
  , { name := "boundary.prompt-assembly.service-tier"
    , reason := "Whether a provider account and model accept a requested service tier, and the speed actually delivered, are decided by the provider; conformance checks only which carriers Gents sends a tier to." }
  , { name := "boundary.rendered-capture.assembled-request-artifact"
    , reason := "Transport capture stores the final HTTP JSON body parsed and canonicalized; equality of captures is canonical JSON equality, so original byte order and whitespace are not retained." }
  , { name := "boundary.rendered-capture.key-encoding-injectivity"
    , reason := "The durable capture key is the SHA-256 digest of a canonical JSON array of the identity tuple; distinct tuples map to distinct keys as long as SHA-256 is collision-free." }
  , { name := "boundary.storage.observation-daemon-visible"
    , reason := "A mutation's success or failure is the acknowledgement DefraDB returns to the caller; the model takes that acknowledgement as given and does not model DefraDB's commit algorithm." }
  , { name := "boundary.tool-policy.path-namespace"
    , reason := "The path guard decides on the paths it inspects; between that inspection and the file operation, another process can change the file namespace, and handle identity across that window is the operating system's." }
  , { name := "boundary.trigger.dispatch-source-delivery"
    , reason := "DefraDB change notifications and host schedule ticks can be delayed or lost; recovery relies on the host running the periodic rescans." }
  , { name := "fold_queue_contracts_input_edit_unreached"
    , reason := "Two physical AgentSessionInputEdit documents never share a command_id: the unique command_id index refuses a second local create, and the collection is managed rather than replicated, so no peer can deliver a twin. The fail-closed twin branch in process_command is defensive." }
  , { name := "node_provisioning_reservation_unreached"
    , reason := "A file sync of a newly created bootstrap marker fails only on an operating-system I/O error, which a test home cannot induce; production propagates that error and the model refuses the reservation." }
  ]

/-- Each name is an emitted family that no native test consumes. The consumer
scan refuses a family missing from this list and a name here that a test
consumes, so the list only shrinks: a new family arrives with its consumer. -/
def openFamilies : List String :=
  [ "apply_reconcile_contract_cases_publish_cases"
  , "client_window_scope_bound_delivers"
  , "contracts_atomic_admission_unreached"
  , "contracts_background_wake_rows_unreached"
  , "contracts_edit_anchor_fork_unreached"
  , "contracts_fork_publication_unreached"
  , "contracts_pack_installation_finality_unreached"
  , "contracts_pack_installation_hosted_plugin_unreached"
  , "contracts_queued_steering_guard_unreached"
  , "contracts_runtime_reconcile_unreached"
  , "contracts_self_config_job_guard_unreached"
  , "contracts_session_advance_unreached"
  , "contracts_session_refresh_unreached"
  , "contracts_title_admission_fresh_verifier_unreached"
  , "contracts_workspace_transition_unreached"
  , "cross_actor_authorization_unreached"
  , "enrollment_auto_renewal_offer_unreached"
  , "eval_records_published_unreached"
  , "event_delivery_group_capture_unreached"
  , "event_delivery_group_clock_unreached"
  , "fold_queue_contracts_management_unreached"
  , "goal_claimed_publication_contracts_identity_staged_unreached"
  , "household_tool_grant_rebinding_unreached"
  , "household_unattended_receipt_unreached"
  , "identity_operations_deferred_unreached"
  , "identity_operations_readiness_observation_unreached"
  , "identity_operations_readiness_recovery_unreached"
  , "mailbox_handoff_contracts_linked_unreached"
  , "native_completion_trace_unreached"
  , "native_refinement_claim_unreached"
  , "node_provisioning_bootstrap_grant_unreached"
  , "remote_delivery_claim_unreached"
  , "remote_delivery_delivery_unreached"
  , "remote_delivery_recovery_unreached"
  , "remote_delivery_response_unreached"
  , "session_child_child_scope_repair_unreached"
  , "session_child_client_request_recovery_unreached"
  , "session_documents_retry_unreached"
  , "title_admission_join_unreached"
  , "title_usage_budget_unreached"
  , "title_usage_catalog_unreached"
  , "title_usage_goal_unreached"
  , "watcher_actors_scanned_unreached"
  ]

/-- A surface a feature reaches, and the consumed family whose replaying test
evidences it. -/
structure SurfaceEvidence where
  surface : Surface
  family : String
  deriving Repr

def SurfaceEvidence.toJson (evidence : SurfaceEvidence) : String :=
  "{"
    ++ "\"surface\":" ++ Surface.toJson evidence.surface ++ ","
    ++ "\"family\":" ++ jsonString evidence.family
    ++ "}"

structure FeatureSurfaceRequirement where
  feature : String
  required : List SurfaceEvidence
  deriving Repr

def FeatureSurfaceRequirement.toJson (req : FeatureSurfaceRequirement) : String :=
  "{"
    ++ "\"feature\":" ++ jsonString req.feature ++ ","
    ++ "\"required\":" ++ jsonArray (req.required.map SurfaceEvidence.toJson)
    ++ "}"

def featureSurfaceRequirements : List FeatureSurfaceRequirement :=
  [ { feature := "request-lifecycle"
    , required :=
        [ ⟨Surface.agentFacing, "request_state_terminal"⟩
        , ⟨Surface.runtimeInternal, "request_state_vocabulary"⟩
        , ⟨Surface.operatorUi, "client_shell_contracts_desktop"⟩
        ]
    }
  , { feature := "request-execution-lease"
    , required :=
        [ ⟨Surface.agentFacing, "request_execution_lease_contracts_provider_eof"⟩
        , ⟨Surface.runtimeInternal, "request_execution_lease_contracts_provider_eof"⟩
        ]
    }
  , { feature := "process-lifecycle"
    , required :=
        [ ⟨Surface.runtimeInternal, "process_state_vocabulary"⟩
        ]
    }
  , { feature := "inference-call"
    , required :=
        [ ⟨Surface.runtimeInternal, "inference_call_state_vocabulary"⟩
        ]
    }
  , { feature := "completion-retry"
    , required :=
        [ ⟨Surface.agentFacing, "count_carrier_value"⟩
        , ⟨Surface.runtimeInternal, "count_carrier_value"⟩
        ]
    }
  , { feature := "account-rotation"
    , required :=
        [ ⟨Surface.runtimeInternal, "account_rotation_contracts_selection"⟩
        ]
    }
  , { feature := "tool-call"
    , required :=
        [ ⟨Surface.agentFacing, "tool_execution_tool_call_state_vocabulary"⟩
        , ⟨Surface.runtimeInternal, "tool_execution_tool_call_state_terminal"⟩
        ]
    }
  , { feature := "composed-invariants"
    , required :=
        [ ⟨Surface.runtimeInternal, "composed_termination_linked"⟩
        ]
    }
  , { feature := "managed-exec"
    , required :=
        [ ⟨Surface.agentFacing, "contracts_managed_exec_liveness_exec"⟩
        ]
    }
  , { feature := "task-hooks"
    , required :=
        [ ⟨Surface.runtimeInternal, "task_hooks_contracts_admission"⟩
        ]
    }
  , { feature := "pairing-reconcile"
    , required :=
        [ ⟨Surface.runtimeInternal, "scope_collection_rules_template_rules"⟩
        ]
    }
  , { feature := "file-inventory"
    , required :=
        [ ⟨Surface.runtimeInternal, "contracts_file_inventory_closure"⟩
        ]
    }
  , { feature := "authenticated-enrollment"
    , required :=
        [ ⟨Surface.runtimeInternal, "enrollment_core_trace"⟩
        , ⟨Surface.operatorUi, "enrollment_pending_requests_startup"⟩
        ]
    }
  , { feature := "runtime-reconcile"
    , required :=
        [ ⟨Surface.runtimeInternal, "reconcile_phase_vocabulary"⟩
        ]
    }
  , { feature := "session-recovery"
    , required :=
        [ ⟨Surface.runtimeInternal, "session_operations_reissue_decision"⟩
        ]
    }
  , { feature := "background-tools"
    , required :=
        [ ⟨Surface.agentFacing, "process_stop_unrecorded_cancel"⟩
        , ⟨Surface.operatorUi, "contracts_tool_output_canonical_source"⟩
        ]
    }
  , { feature := "interrupt-and-cancel"
    , required :=
        [ ⟨Surface.agentFacing, "tool_execution_cancel_cause_vocabulary"⟩
        , ⟨Surface.operatorUi, "tool_execution_cancel_cause_vocabulary"⟩
        ]
    }
  , { feature := "session-control"
    , required :=
        [ ⟨Surface.agentFacing, "session_control_contracts_interrupt_idle"⟩
        , ⟨Surface.runtimeInternal, "session_control_contracts_interrupt_idle"⟩
        ]
    }
  , { feature := "lifecycle-hooks"
    , required :=
        [ ⟨Surface.runtimeInternal, "hooks_contracts_owed_trace"⟩
        ]
    }
  , { feature := "mcp-health"
    , required :=
        [ ⟨Surface.runtimeInternal, "contracts_mcp_health_transition"⟩
        , ⟨Surface.operatorUi, "contracts_mcp_health_view"⟩
        ]
    }
  , { feature := "identity-permission"
    , required :=
        [ ⟨Surface.runtimeInternal, "client_agent_journal_recovery"⟩
        , ⟨Surface.api, "agent_identity_permission_decide"⟩
        ]
    }
  , { feature := "apply-reconcile"
    , required :=
        [ ⟨Surface.operatorCli, "initialization_initial_inference"⟩
        ]
    }
  , { feature := "eval"
    , required :=
        [ ⟨Surface.operatorCli, "eval_outcome_kind_vocabulary"⟩
        ]
    }
  , { feature := "optimization"
    , required :=
        [ ⟨Surface.operatorCli, "optimization_alpha_effective"⟩
        ]
    }
  , { feature := "event-delivery"
    , required :=
        [ ⟨Surface.runtimeInternal, "trigger_delivery_arrival_cursor"⟩
        ]
    }
  , { feature := "triggers"
    , required :=
        [ ⟨Surface.runtimeInternal, "trigger_delivery_fire_admission"⟩
        , ⟨Surface.operatorCli, "trigger_contracts_manual_run"⟩
        , ⟨Surface.operatorUi, "trigger_contracts_recent_run"⟩
        ]
    }
  , { feature := "compaction"
    , required :=
        [ ⟨Surface.agentFacing, "contracts_compaction_reducer_gate"⟩
        ]
    }
  , { feature := "transcript"
    , required :=
        [ ⟨Surface.agentFacing, "transcript_cases_native_completion"⟩
        , ⟨Surface.operatorUi, "transcript_cases_snapshot_projection"⟩
        ]
    }
  , { feature := "prompt-assembly"
    , required :=
        [ ⟨Surface.agentFacing, "contracts_queued_steering_trace_publication"⟩
        , ⟨Surface.runtimeInternal, "compaction_checkpoint_claude_output_config"⟩
        ]
    }
  , { feature := "rendered-capture"
    , required :=
        [ ⟨Surface.runtimeInternal, "contracts_rendered_capture"⟩
        , ⟨Surface.operatorCli, "contracts_rendered_capture"⟩
        , ⟨Surface.operatorUi, "contracts_rendered_capture"⟩
        ]
    }
  , { feature := "canonical-output"
    , required :=
        [ ⟨Surface.runtimeInternal, "worker_capacity_contracts_transition"⟩
        , ⟨Surface.agentFacing, "native_execution_contracts_script"⟩
        , ⟨Surface.operatorUi, "title_text_summary"⟩
        ]
    }
  , { feature := "client-shell"
    , required :=
        [ ⟨Surface.operatorUi, "contracts_request_progress"⟩
        ]
    }
  , { feature := "durable-goals"
    , required :=
        [ ⟨Surface.agentFacing, "contracts_goal_create"⟩
        , ⟨Surface.runtimeInternal, "goals_status_vocabulary"⟩
        , ⟨Surface.operatorCli, "goal_delivery_input_manual_task_run"⟩
        , ⟨Surface.operatorUi, "goal_session_observation_contracts_presentation"⟩
        ]
    }
  , { feature := "command-policy"
    , required :=
        [ ⟨Surface.agentFacing, "contracts_command_policy_validation"⟩
        , ⟨Surface.operatorUi, "contracts_command_policy_validation"⟩
        ]
    }
  , { feature := "tool-policy"
    , required :=
        [ ⟨Surface.agentFacing, "contracts_tool_policy_composition"⟩
        , ⟨Surface.operatorUi, "contracts_tool_policy_composition"⟩
        ]
    }
  , { feature := "self-config"
    , required :=
        [ ⟨Surface.agentFacing, "contracts_self_config_field_table"⟩
        ]
    }
  , { feature := "recovery"
    , required :=
        [ ⟨Surface.runtimeInternal, "recovery_sweeps_request"⟩
        ]
    }
  , { feature := "fleet-slot-accounting"
    , required :=
        [ ⟨Surface.runtimeInternal, "fleet_slot_accounting_contribution"⟩
        , ⟨Surface.api, "fleet_slot_accounting_bound"⟩
        ]
    }
  , { feature := "storage-observation"
    , required :=
        [ ⟨Surface.runtimeInternal, "hook_persistence_mutation"⟩
        ]
    }
  , { feature := "persistence-failure-policy"
    , required :=
        [ ⟨Surface.runtimeInternal, "persistence_state_failure_policy_vocabulary"⟩
        ]
    }
  , { feature := "backend-health"
    , required :=
        [ ⟨Surface.runtimeInternal, "contracts_backend_health_transition"⟩
        , ⟨Surface.operatorCli, "contracts_backend_health_transition"⟩
        , ⟨Surface.operatorUi, "backend_health_admission_display"⟩
        ]
    }
  , { feature := "isolated-workspaces"
    , required :=
        [ ⟨Surface.runtimeInternal, "operator_base_freeze_contracts_freeze"⟩
        ]
    }
  , { feature := "mailbox"
    , required :=
        [ ⟨Surface.agentFacing, "mailbox_status_vocabulary"⟩
        , ⟨Surface.operatorCli, "mailbox_status_vocabulary"⟩
        , ⟨Surface.operatorUi, "mailbox_status_vocabulary"⟩
        , ⟨Surface.api, "mailbox_status_vocabulary"⟩
        , ⟨Surface.runtimeInternal, "mailbox_status_vocabulary"⟩
        ]
    }
  , { feature := "plugin-jobs"
    , required :=
        [ ⟨Surface.agentFacing, "plugin_job_source_boundary_dispatch"⟩
        , ⟨Surface.runtimeInternal, "plugin_job_admission"⟩
        , ⟨Surface.operatorCli, "plugin_job_lineage_starting"⟩
        , ⟨Surface.operatorUi, "plugin_job_lineage_opening"⟩
        ]
    }
  ]

def coverageLedgerJson : String :=
  "{"
    ++ "\"boundaries\":" ++ jsonArray (boundaries.map Boundary.toJson) ++ ","
    ++ "\"open\":" ++ jsonStringArray openFamilies
    ++ "}"

def featureSurfaceRequirementsJson : String :=
  jsonArray (featureSurfaceRequirements.map FeatureSurfaceRequirement.toJson)

theorem boundaries_have_reasons :
    boundaries.all (fun boundary => !boundary.reason.isEmpty) = true := by native_decide

theorem ledger_names_are_distinct :
    ((boundaries.map Boundary.name) ++ openFamilies).Nodup := by native_decide

theorem features_are_distinct :
    (featureSurfaceRequirements.map FeatureSurfaceRequirement.feature).Nodup := by native_decide

end Conformance.Contracts
