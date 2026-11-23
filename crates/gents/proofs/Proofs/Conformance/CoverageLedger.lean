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
    , reason := "A successful models-endpoint probe shows only that the provider's endpoint answered; whether its other operations work, whether it stays reachable and whether every replica is fresh are the provider's." }
  , { name := "boundary.command-policy.host-execution-assumptions"
    , reason := "External binaries do what their declared effects say, and the host kernel enforces the sandbox the command policy selects; `workspace_write_bash_contains_writes_to_tool_root` evidences the macOS Seatbelt refusing a write outside the tool root." }
  , { name := "boundary.count-carrier.non-null-id"
    , reason := "DefraDB refuses a non-null ID field declaration, so no count carrier of that schema is ever stored; `defradb_refuses_a_non_null_id_count_carrier` evidences the refusal." }
  , { name := "boundary.event-delivery.fair-substrate"
    , reason := "Delivery liveness assumes the database and the host scheduler eventually run every rescan and handler; scheduler fairness is not a property a finite trace can show." }
  , { name := "boundary.model.nat-typed-ids-time"
    , reason := "Models represent identifiers and times as natural numbers: native identifiers are taken as unique and faithfully parsed, and the clocks of different hosts are not assumed to agree." }
  , { name := "boundary.p2p-backpressure.obligation-model"
    , reason := "P2PBackpressure models one bounded wave; the database's multi-wave queue, durable retry ladder, store-before-acknowledge and restart behaviour are defradb.rs's and are modelled there." }
  , { name := "boundary.plugin-data.transaction"
    , reason := "DefraDB transactions are serializable and enforce unique indexes, so of two transactions that read a unique key as absent and insert it, one commits and the other is refused; `defradb_refuses_a_concurrent_duplicate_after_read_then_insert` evidences it." }
  , { name := "boundary.plugin-job.guest-determinism"
    , reason := "The WASM guest supplies its step function, output schema and resume bookkeeping; core takes the guest's behaviour as given and does not establish its determinism." }
  , { name := "boundary.plugin-job.single-step-driver"
    , reason := "A node's store admits one runtime process at a time through an operating-system advisory lock, which in-process plugin job step coalescing relies on; `a_second_holder_cannot_lock_a_store` evidences the lock." }
  , { name := "boundary.prompt-assembly.service-tier"
    , reason := "Whether a provider account and model accept a requested service tier, and the speed delivered, are the provider's; conformance checks only which carriers Gents sends a tier to." }
  , { name := "boundary.rendered-capture.key-encoding-injectivity"
    , reason := "The durable capture key is the SHA-256 digest of a canonical JSON array of the identity tuple, so distinct tuples have distinct keys while SHA-256 is collision-free." }
  , { name := "boundary.storage.observation-daemon-visible"
    , reason := "A mutation's success or failure is the acknowledgement DefraDB returns, and models take it as given rather than modelling DefraDB's commit algorithm; `defradb_refuses_a_concurrent_duplicate_after_read_then_insert` evidences an acknowledged commit read back and a refused one leaving nothing." }
  , { name := "boundary.tool-policy.path-namespace"
    , reason := "Between the path guard's inspection and the file operation another process can change the file namespace; handle identity across that window is the operating system's." }
  , { name := "boundary.trigger.dispatch-source-delivery"
    , reason := "DefraDB change notifications and subscription events can be delayed, dropped or redelivered, and host schedule ticks can be late or missed; delivery recovers by the periodic rescans the host runs, and native delivery families observe emitted requests and local suppression, not the transport queue." }
  ]

/-- Each name is an emitted family that no native test consumes. The consumer
scan refuses a family missing from this list and a name here that a test
consumes, so the list only shrinks: a new family arrives with its consumer. -/
def openFamilies : List String :=
  [ "apply_reconcile_contract_cases_publish_cases"
  , "contracts_atomic_admission_unreached"
  , "contracts_pack_installation_hosted_plugin_unreached"
  , "contracts_runtime_reconcile_unreached"
  , "contracts_self_config_job_guard_unreached"
  , "cross_actor_authorization_unreached"
  , "enrollment_auto_renewal_offer_unreached"
  , "event_delivery_group_capture_unreached"
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
