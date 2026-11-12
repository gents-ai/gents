import Proofs.Session.RequestExecutionLease
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.RequestExecutionLeaseContracts

open Conformance.Contracts
open RequestExecutionLease

abbrev Generation := Nat

private def world
    (request : RequestState) (lease : Lease Generation)
    (usedGenerations : List Generation) (now : Nat)
    (continuationRequired tokenChargeRequired : Bool := true)
    (continuationCount tokenChargeCount : Nat := 0) : World Generation :=
  { request
  , lease
  , usedGenerations
  , now
  , continuationRequired
  , tokenChargeRequired
  , continuationCount
  , tokenChargeCount
  }

private def vacant : World Generation :=
  world .pending .vacant [] 0

private def claimed
    (generation duration deadline now : Nat := 1) : World Generation :=
  world .claimed (.active generation duration deadline) [generation] now

private def processing (generation duration deadline now : Nat := 1) : World Generation :=
  world .processing (.active generation duration deadline) [generation] now

private def recoverable (generation duration deadline now : Nat := 1) : World Generation :=
  world .processing (.recoverable generation duration deadline) [generation] now

private def recovered (oldGeneration generation duration deadline now : Nat) : World Generation :=
  world .processing (.active generation duration deadline)
    [generation, oldGeneration] now

structure LeaseCase where
  name : String
  pre : World Generation
  action : Action Generation
  expected : Option (World Generation)
  deriving DecidableEq, Repr

def leaseCases : List LeaseCase :=
  [ { name := "fresh_claim_installs_generation_duration_and_deadline"
    , pre := vacant
    , action := .claim .mutationWriteGate 101 5 10
    , expected := some (claimed 101 5 10 0)
    }
  , { name := "claim_rejects_reused_generation"
    , pre := { vacant with usedGenerations := [101] }
    , action := .claim .mutationWriteGate 101 5 10
    , expected := none
    }
  , { name := "claim_rejects_zero_duration"
    , pre := vacant
    , action := .claim .mutationWriteGate 101 0 10
    , expected := none
    }
  , { name := "observing_replica_cannot_claim"
    , pre := vacant
    , action := .claim .observingReplica 101 5 10
    , expected := none
    }
  , { name := "matching_owner_begins_processing"
    , pre := claimed 101 5 10 1
    , action := .begin .mutationWriteGate 101
    , expected := some (processing 101 5 10 1)
    }
  , { name := "output_append_authorization_does_not_renew"
    , pre := processing 101 5 10 4
    , action := .appendOutput .mutationWriteGate 101
    , expected := some (processing 101 5 10 4)
    }
  , { name := "socket_traffic_is_not_progress"
    , pre := processing 101 5 10 12
    , action := .socketTraffic 101
    , expected := some (processing 101 5 10 12)
    }
  , { name := "no_op_is_not_progress"
    , pre := processing 101 5 10 12
    , action := .noOp 101
    , expected := some (processing 101 5 10 12)
    }
  , { name := "stale_generation_cannot_append"
    , pre := { processing 202 5 20 5 with usedGenerations := [202, 101] }
    , action := .appendOutput .mutationWriteGate 101
    , expected := none
    }
  , { name := "stale_generation_cannot_renew"
    , pre := { processing 202 5 20 5 with usedGenerations := [202, 101] }
    , action := .renew .mutationWriteGate 101 20
    , expected := none
    }
  , { name := "stale_generation_cannot_finalize"
    , pre := { processing 202 5 20 5 with usedGenerations := [202, 101] }
    , action := .finalize .mutationWriteGate 101 .completed
    , expected := none
    }
  , { name := "deadline_boundary_rejects_raw_append"
    , pre := processing 101 5 10 10
    , action := .appendOutput .mutationWriteGate 101
    , expected := none
    }
  , { name := "deadline_boundary_rejects_explicit_renewal"
    , pre := processing 101 5 10 10
    , action := .renew .mutationWriteGate 101 10
    , expected := none
    }
  , { name := "deadline_boundary_rejects_acceptance_publication_authorization"
    , pre := processing 101 5 10 10
    , action := .authorizeProducerDecision .mutationWriteGate 101 .acceptAndPublish
    , expected := none
    }
  , { name := "deadline_boundary_rejects_dispatch_authorization"
    , pre := processing 101 5 10 10
    , action := .authorizeProducerDecision .mutationWriteGate 101 .dispatch
    , expected := none
    }
  , { name := "deadline_boundary_rejects_terminalization"
    , pre := processing 101 5 10 10
    , action := .finalize .mutationWriteGate 101 .completed
    , expected := none
    }
  , { name := "deadline_boundary_atomically_recovers_fresh_generation"
    , pre := processing 101 5 10 10
    , action := .recoverExpired .mutationWriteGate 101 202 5 30
    , expected := some (recovered 101 202 5 30 10)
    }
  , { name := "observing_replica_cannot_recover_expired_work"
    , pre := processing 101 5 10 11
    , action := .recoverExpired .observingReplica 101 202 5 30
    , expected := none
    }
  , { name := "observing_replica_cannot_append"
    , pre := processing 101 5 10 5
    , action := .appendOutput .observingReplica 101
    , expected := none
    }
  , { name := "explicit_owner_heartbeat_extends_at_due_time"
    , pre := processing 101 5 10 8
    , action := .renew .mutationWriteGate 101 10
    , expected := some (processing 101 5 13 8)
    }
  , { name := "early_renewal_is_rejected"
    , pre := processing 101 5 10 5
    , action := .renew .mutationWriteGate 101 10
    , expected := none
    }
  , { name := "stale_expected_deadline_cannot_renew"
    , pre := processing 101 5 10 8
    , action := .renew .mutationWriteGate 101 9
    , expected := none
    }
  , { name := "same_deadline_replay_after_renewal_is_rejected"
    , pre := processing 101 5 13 8
    , action := .renew .mutationWriteGate 101 10
    , expected := none
    }
  , { name := "one_tick_duration_cannot_advance_before_expiry"
    , pre := processing 101 1 10 9
    , action := .renew .mutationWriteGate 101 10
    , expected := none
    }
  , { name := "terminal_lifecycle_rejects_renewal_even_with_active_lease"
    , pre := { processing 101 5 10 8 with request := .completed }
    , action := .renew .mutationWriteGate 101 10
    , expected := none
    }
  , { name := "close_or_retract_authorization_does_not_renew"
    , pre := processing 101 5 10 5
    , action := .authorizeProducerDecision .mutationWriteGate 101 .closeOrRetract
    , expected := some (processing 101 5 10 5)
    }
  , { name := "accept_and_publish_authorization_does_not_renew"
    , pre := processing 101 5 10 5
    , action := .authorizeProducerDecision .mutationWriteGate 101 .acceptAndPublish
    , expected := some (processing 101 5 10 5)
    }
  , { name := "dispatch_authorization_does_not_renew"
    , pre := processing 101 5 10 5
    , action := .authorizeProducerDecision .mutationWriteGate 101 .dispatch
    , expected := some (processing 101 5 10 5)
    }
  , { name := "drop_relinquishes_for_distinct_recovery_path"
    , pre := processing 101 5 10 5
    , action := .drop .mutationWriteGate 101
    , expected := some (recoverable 101 5 10 5)
    }
  , { name := "expired_drop_preserves_the_authoritative_deadline"
    , pre := processing 101 5 10 11
    , action := .drop .mutationWriteGate 101
    , expected := some (processing 101 5 10 11)
    }
  , { name := "drop_at_expiry_preserves_the_authoritative_deadline"
    , pre := processing 101 5 10 10
    , action := .drop .mutationWriteGate 101
    , expected := some (processing 101 5 10 10)
    }
  , { name := "dropped_recovery_takes_fresh_generation"
    , pre := recoverable 101 5 10 11
    , action := .recoverDropped .mutationWriteGate 101 202 5 30
    , expected := some (recovered 101 202 5 30 11)
    }
  , { name := "dropped_recovery_rejects_zero_duration"
    , pre := recoverable 101 5 10 11
    , action := .recoverDropped .mutationWriteGate 101 202 0 30
    , expected := none
    }
  , { name := "dropped_recovery_rejects_wrong_expected_generation"
    , pre := recoverable 101 5 10 11
    , action := .recoverDropped .mutationWriteGate 999 202 5 30
    , expected := none
    }
  , { name := "dropped_recovery_rejects_aba_generation_reuse"
    , pre := { recoverable 202 5 20 21 with usedGenerations := [202, 101] }
    , action := .recoverDropped .mutationWriteGate 202 101 5 30
    , expected := none
    }
  , { name := "expired_recovery_rejects_wrong_expected_generation"
    , pre := processing 101 5 10 11
    , action := .recoverExpired .mutationWriteGate 999 202 5 30
    , expected := none
    }
  , { name := "expired_recovery_rejects_aba_generation_reuse"
    , pre := { processing 202 5 20 21 with usedGenerations := [202, 101] }
    , action := .recoverExpired .mutationWriteGate 202 101 5 30
    , expected := none
    }
  , { name := "expired_recovery_failure_atomically_elects_terminal_winner"
    , pre := processing 101 5 10 11
    , action := .recoverExpiredAndFail .mutationWriteGate 101 202
    , expected := some
        (world .failed (.terminal 202 .failed) [202, 101] 11 true true 1 1)
    }
  , { name := "expired_terminal_recovery_preserves_failure_contract"
    , pre := processing 101 5 10 11
    , action := .recoverExpiredTerminal .mutationWriteGate 101 202 .failed
    , expected := some
        (world .failed (.terminal 202 .failed) [202, 101] 11 true true 1 1)
    }
  , { name := "expired_terminal_recovery_preserves_interrupt_contract"
    , pre := processing 101 5 10 11
    , action := .recoverExpiredTerminal .mutationWriteGate 101 202 .interrupted
    , expected := some
        (world .interrupted (.terminal 202 .interrupted) [202, 101] 11 true true 1 1)
    }
  , { name := "expired_terminal_recovery_rejects_completed_outcome"
    , pre := processing 101 5 10 11
    , action := .recoverExpiredTerminal .mutationWriteGate 101 202 .completed
    , expected := none
    }
  , { name := "dropped_recovery_failure_atomically_elects_terminal_winner"
    , pre := recoverable 101 5 10 11
    , action := .recoverDroppedAndFail .mutationWriteGate 101 202
    , expected := some
        (world .failed (.terminal 202 .failed) [202, 101] 11 true true 1 1)
    }
  , { name := "completion_atomically_terminalizes_request"
    , pre := processing 101 5 10 5
    , action := .finalize .mutationWriteGate 101 .completed
    , expected := some
        (world .completed (.terminal 101 .completed) [101] 5 true true 1 1)
    }
  , { name := "provider_eof_fails_claimed_request_atomically"
    , pre := claimed 101 5 10 5
    , action := .finalize .mutationWriteGate 101 .failed
    , expected := some
        (world .failed (.terminal 101 .failed) [101] 5 true true 1 1)
    }
  , { name := "completion_rejects_claimed_request"
    , pre := claimed 101 5 10 5
    , action := .finalize .mutationWriteGate 101 .completed
    , expected := none
    }
  , { name := "policy_authority_can_supersede_live_generation"
    , pre := processing 101 5 10 5
    , action := .policyRevoke .mutationWriteGate 101 202 .superseded
    , expected := some
        (world .superseded (.terminal 202 .superseded) [202, 101] 5 true true 1 1)
    }
  , { name := "policy_revocation_rejects_wrong_expected_generation"
    , pre := processing 101 5 10 5
    , action := .policyRevoke .mutationWriteGate 999 202 .dead
    , expected := none
    }
  , { name := "policy_revocation_rejects_non_policy_outcome"
    , pre := processing 101 5 10 5
    , action := .policyRevoke .mutationWriteGate 101 202 .failed
    , expected := none
    }
  , { name := "clock_cannot_move_backwards"
    , pre := processing 101 5 10 5
    , action := .advanceTime 4
    , expected := none
    }
  ]

theorem leaseCases_count : leaseCases.length = 49 := by native_decide

theorem leaseCases_hold :
    leaseCases.all (fun testCase =>
      step? testCase.pre testCase.action == testCase.expected) = true := by
  native_decide

structure LeaseTraceCase where
  name : String
  pre : World Generation
  actions : List (Action Generation)
  expected : Option (World Generation)
  deriving DecidableEq, Repr

def leaseTraceCases : List LeaseTraceCase :=
  [ { name := "traffic_cannot_prevent_atomic_boundary_recovery"
    , pre := processing 101 5 10 9
    , actions :=
        [ .socketTraffic 101
        , .noOp 101
        , .advanceTime 10
        , .recoverExpiredAndFail .mutationWriteGate 101 202
        ]
    , expected := some
        (world .failed (.terminal 202 .failed) [202, 101] 10 true true 1 1)
    }
  , { name := "output_requires_separate_explicit_renewal"
    , pre := processing 101 5 10 9
    , actions :=
        [ .appendOutput .mutationWriteGate 101
        , .advanceTime 11
        , .finalize .mutationWriteGate 101 .completed
        ]
    , expected := none
    }
  , { name := "expired_expected_deadline_does_not_revive_generation"
    , pre := processing 101 5 10 9
    , actions :=
        [ .advanceTime 10
        , .renew .mutationWriteGate 101 10
        ]
    , expected := none
    }
  , { name := "explicit_slow_heartbeat_renews_then_completes"
    , pre := processing 101 5 10 8
    , actions :=
        [ .renew .mutationWriteGate 101 10
        , .advanceTime 12
        , .finalize .mutationWriteGate 101 .completed
        ]
    , expected := some
        (world .completed (.terminal 101 .completed) [101] 12 true true 1 1)
    }
  , { name := "duplicate_renewal_with_consumed_deadline_loses_cas"
    , pre := processing 101 5 10 8
    , actions :=
        [ .renew .mutationWriteGate 101 10
        , .renew .mutationWriteGate 101 10
        ]
    , expected := none
    }
  , { name := "output_write_and_explicit_heartbeat_are_separate"
    , pre := processing 101 5 10 9
    , actions :=
        [ .appendOutput .mutationWriteGate 101
        , .renew .mutationWriteGate 101 10
        , .advanceTime 11
        , .finalize .mutationWriteGate 101 .completed
        ]
    , expected := some
        (world .completed (.terminal 101 .completed) [101] 11 true true 1 1)
    }
  , { name := "atomically_recovered_owner_wins_and_stale_owner_cannot_finalize"
    , pre := processing 101 5 10 11
    , actions :=
        [ .recoverExpired .mutationWriteGate 101 202 5 30
        , .finalize .mutationWriteGate 101 .completed
        ]
    , expected := none
    }
  , { name := "producer_authorization_does_not_claim_canonical_source_effects"
    , pre := processing 101 5 10 5
    , actions :=
        [ .authorizeProducerDecision .mutationWriteGate 101 .closeOrRetract
        , .appendOutput .mutationWriteGate 101
        , .authorizeProducerDecision .mutationWriteGate 101 .acceptAndPublish
        ]
    , expected := some (processing 101 5 10 5)
    }
  ]

theorem leaseTraceCases_count : leaseTraceCases.length = 8 := by native_decide

theorem leaseTraceCases_hold :
    leaseTraceCases.all (fun testCase =>
      replay? testCase.pre testCase.actions == testCase.expected) = true := by
  native_decide

private def boolJson (value : Bool) : String :=
  if value then "true" else "false"

def outcomeName : Outcome → String
  | .completed => "completed"
  | .failed => "failed"
  | .interrupted => "interrupted"
  | .dead => "dead"
  | .superseded => "superseded"

def boundaryName : Boundary → String
  | .mutationWriteGate => "mutation_write_gate"
  | .validatedTransaction => "validated_transaction"
  | .observingReplica => "observing_replica"

def decisionName : ProducerDecision → String
  | .closeOrRetract => "close_or_retract"
  | .acceptAndPublish => "accept_and_publish"
  | .dispatch => "dispatch"

def leaseJson : Lease Generation → String
  | .vacant =>
      "{\"status\":\"vacant\",\"generation\":null,\"duration\":null," ++
        "\"explicit_deadline\":null,\"outcome\":null}"
  | .active generation duration deadline =>
      "{\"status\":\"active\",\"generation\":" ++ toString generation ++
        ",\"duration\":" ++ toString duration ++
        ",\"explicit_deadline\":" ++ toString deadline ++ ",\"outcome\":null}"
  | .recoverable generation duration deadline =>
      "{\"status\":\"recoverable\",\"generation\":" ++ toString generation ++
        ",\"duration\":" ++ toString duration ++
        ",\"explicit_deadline\":" ++ toString deadline ++ ",\"outcome\":null}"
  | .terminal generation outcome =>
      "{\"status\":\"terminal\",\"generation\":" ++ toString generation ++
        ",\"duration\":null,\"explicit_deadline\":null,\"outcome\":" ++
        jsonString (outcomeName outcome) ++ "}"

def worldJson (value : World Generation) : String :=
  "{"
    ++ "\"request\":" ++ jsonString (value.request.toDefraDB) ++ ","
    ++ "\"lease\":" ++ leaseJson value.lease ++ ","
    ++ "\"used_generations\":" ++
      jsonArray (value.usedGenerations.map (fun generation => toString generation)) ++ ","
    ++ "\"now\":" ++ toString value.now ++ ","
    ++ "\"effective_expiry\":" ++ toString (effectiveExpiry value) ++ ","
    ++ "\"continuation_required\":" ++ boolJson value.continuationRequired ++ ","
    ++ "\"token_charge_required\":" ++ boolJson value.tokenChargeRequired ++ ","
    ++ "\"continuation_count\":" ++ toString value.continuationCount ++ ","
    ++ "\"token_charge_count\":" ++ toString value.tokenChargeCount
    ++ "}"

def optionalWorldJson : Option (World Generation) → String
  | none => "null"
  | some value => worldJson value

def actionJson : Action Generation → String
  | .reparkExpiredResume boundary generation deadline =>
      "{\"kind\":\"repark_expired_resume\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ ",\"expected_deadline\":" ++ toString deadline ++ "}"
  | .parkBackend boundary generation closed =>
      "{\"kind\":\"park_backend\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ ",\"closed\":" ++
        (if closed then "true" else "false") ++ "}"
  | .resumeBackend boundary generation duration deadline wake =>
      "{\"kind\":\"resume_backend\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ ",\"duration\":" ++ toString duration ++
        ",\"explicit_deadline\":" ++ toString deadline ++ ",\"wake\":" ++
        (if wake then "true" else "false") ++ "}"
  | .finalizeParked boundary generation outcome =>
      "{\"kind\":\"finalize_parked\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ ",\"outcome\":" ++
        jsonString (outcomeName outcome) ++ "}"
  | .claim boundary generation duration deadline =>
      "{\"kind\":\"claim\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++
        ",\"duration\":" ++ toString duration ++
        ",\"explicit_deadline\":" ++ toString deadline ++ "}"
  | .begin boundary generation =>
      "{\"kind\":\"begin\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ "}"
  | .appendOutput boundary generation =>
      "{\"kind\":\"append_output\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ "}"
  | .renew boundary generation expectedDeadline =>
      "{\"kind\":\"renew\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++
        ",\"expected_deadline\":" ++ toString expectedDeadline ++ "}"
  | .authorizeProducerDecision boundary generation decision =>
      "{\"kind\":\"authorize_producer_decision\",\"boundary\":" ++
        jsonString (boundaryName boundary) ++ ",\"generation\":" ++
        toString generation ++ ",\"decision\":" ++ jsonString (decisionName decision) ++ "}"
  | .socketTraffic generation =>
      "{\"kind\":\"socket_traffic\",\"generation\":" ++ toString generation ++ "}"
  | .noOp generation =>
      "{\"kind\":\"no_op\",\"generation\":" ++ toString generation ++ "}"
  | .advanceTime now => "{\"kind\":\"advance_time\",\"now\":" ++ toString now ++ "}"
  | .drop boundary generation =>
      "{\"kind\":\"drop\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ "}"
  | .recoverExpired boundary expected fresh duration deadline =>
      "{\"kind\":\"recover_expired\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"expected_generation\":" ++ toString expected ++
        ",\"fresh_generation\":" ++ toString fresh ++
        ",\"duration\":" ++ toString duration ++
        ",\"explicit_deadline\":" ++ toString deadline ++ "}"
  | .recoverDropped boundary expected fresh duration deadline =>
      "{\"kind\":\"recover_dropped\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"expected_generation\":" ++ toString expected ++
        ",\"fresh_generation\":" ++ toString fresh ++
        ",\"duration\":" ++ toString duration ++
        ",\"explicit_deadline\":" ++ toString deadline ++ "}"
  | .finalize boundary generation outcome =>
      "{\"kind\":\"finalize\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++
        ",\"outcome\":" ++ jsonString (outcomeName outcome) ++ "}"
  | .policyRevoke boundary expected fresh outcome =>
      "{\"kind\":\"policy_revoke\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"expected_generation\":" ++ toString expected ++
        ",\"fresh_generation\":" ++ toString fresh ++
        ",\"outcome\":" ++ jsonString (outcomeName outcome) ++ "}"
  | .recoverExpiredAndFail boundary expected fresh =>
      "{\"kind\":\"recover_expired_and_fail\",\"boundary\":" ++
        jsonString (boundaryName boundary) ++ ",\"expected_generation\":" ++
        toString expected ++ ",\"fresh_generation\":" ++ toString fresh ++ "}"
  | .recoverExpiredTerminal boundary expected fresh outcome =>
      "{\"kind\":\"recover_expired_terminal\",\"boundary\":" ++
        jsonString (boundaryName boundary) ++ ",\"expected_generation\":" ++
        toString expected ++ ",\"fresh_generation\":" ++ toString fresh ++
        ",\"outcome\":" ++ jsonString (outcomeName outcome) ++ "}"
  | .recoverDroppedAndFail boundary expected fresh =>
      "{\"kind\":\"recover_dropped_and_fail\",\"boundary\":" ++
        jsonString (boundaryName boundary) ++ ",\"expected_generation\":" ++
        toString expected ++ ",\"fresh_generation\":" ++ toString fresh ++ "}"

structure LeaseWire where
  status : String
  generation : Option Nat
  duration : Option Nat
  explicitDeadline : Option Nat
  outcome : Option String
  deriving Lean.ToJson, DecidableEq

def LeaseWire.ofModel : Lease Generation → LeaseWire
  | .vacant => ⟨"vacant", none, none, none, none⟩
  | .active generation duration deadline =>
      ⟨"active", some generation, some duration, some deadline, none⟩
  | .recoverable generation duration deadline =>
      ⟨"recoverable", some generation, some duration, some deadline, none⟩
  | .terminal generation outcome => ⟨"terminal", some generation, none, none, some (outcomeName outcome)⟩

def outcomeOf : String → Outcome
  | "failed" => .failed
  | "interrupted" => .interrupted
  | "dead" => .dead
  | "superseded" => .superseded
  | _ => .completed

def boundaryOf : String → Boundary
  | "validated_transaction" => .validatedTransaction
  | "observing_replica" => .observingReplica
  | _ => .mutationWriteGate

def decisionOf : String → ProducerDecision
  | "accept_and_publish" => .acceptAndPublish
  | "dispatch" => .dispatch
  | _ => .closeOrRetract

def LeaseWire.toModel (wire : LeaseWire) : Lease Generation :=
  let generation := wire.generation.getD 0
  let duration := wire.duration.getD 0
  let deadline := wire.explicitDeadline.getD 0
  match wire.status with
  | "active" => .active generation duration deadline
  | "recoverable" => .recoverable generation duration deadline
  | "terminal" => .terminal generation (outcomeOf (wire.outcome.getD ""))
  | _ => .vacant

structure WorldWire where
  request : RequestState
  lease : LeaseWire
  usedGenerations : List Nat
  now : Nat
  effectiveExpiry : Nat
  continuationRequired : Bool
  tokenChargeRequired : Bool
  continuationCount : Nat
  tokenChargeCount : Nat
  deriving Lean.ToJson

def WorldWire.ofModel (world : World Generation) : WorldWire :=
  { request := world.request
    lease := LeaseWire.ofModel world.lease
    usedGenerations := world.usedGenerations
    now := world.now
    effectiveExpiry := RequestExecutionLease.effectiveExpiry world
    continuationRequired := world.continuationRequired
    tokenChargeRequired := world.tokenChargeRequired
    continuationCount := world.continuationCount
    tokenChargeCount := world.tokenChargeCount }

def WorldWire.toModel (wire : WorldWire) : World Generation :=
  { request := wire.request
    lease := wire.lease.toModel
    usedGenerations := wire.usedGenerations
    now := wire.now
    continuationRequired := wire.continuationRequired
    tokenChargeRequired := wire.tokenChargeRequired
    continuationCount := wire.continuationCount
    tokenChargeCount := wire.tokenChargeCount }

structure ActionWire where
  kind : String
  boundary : Option String := none
  generation : Option Nat := none
  duration : Option Nat := none
  explicitDeadline : Option Nat := none
  expectedDeadline : Option Nat := none
  decision : Option String := none
  now : Option Nat := none
  expectedGeneration : Option Nat := none
  freshGeneration : Option Nat := none
  outcome : Option String := none
  closed : Option Bool := none
  wake : Option Bool := none
  deriving Lean.ToJson

def ActionWire.ofModel : Action Generation → ActionWire
  | .claim boundary generation duration deadline =>
      { kind := "claim", boundary := boundaryName boundary, generation := generation,
        duration := duration, explicitDeadline := deadline }
  | .parkBackend boundary generation closed =>
      { kind := "park_backend", boundary := boundaryName boundary, generation := generation,
        closed := closed }
  | .resumeBackend boundary generation duration deadline wake =>
      { kind := "resume_backend", boundary := boundaryName boundary, generation := generation,
        duration := duration, explicitDeadline := deadline, wake := wake }
  | .reparkExpiredResume boundary generation deadline =>
      { kind := "repark_expired_resume", boundary := boundaryName boundary,
        generation := generation, expectedDeadline := deadline }
  | .finalizeParked boundary generation outcome =>
      { kind := "finalize_parked", boundary := boundaryName boundary, generation := generation,
        outcome := outcomeName outcome }
  | .begin boundary generation =>
      { kind := "begin", boundary := boundaryName boundary, generation := generation }
  | .appendOutput boundary generation =>
      { kind := "append_output", boundary := boundaryName boundary, generation := generation }
  | .renew boundary generation deadline =>
      { kind := "renew", boundary := boundaryName boundary, generation := generation,
        expectedDeadline := deadline }
  | .authorizeProducerDecision boundary generation decision =>
      { kind := "authorize_producer_decision", boundary := boundaryName boundary,
        generation := generation, decision := decisionName decision }
  | .socketTraffic generation => { kind := "socket_traffic", generation := generation }
  | .noOp generation => { kind := "no_op", generation := generation }
  | .advanceTime now => { kind := "advance_time", now := now }
  | .drop boundary generation =>
      { kind := "drop", boundary := boundaryName boundary, generation := generation }
  | .recoverExpired boundary expected fresh duration deadline =>
      { kind := "recover_expired", boundary := boundaryName boundary,
        expectedGeneration := expected, freshGeneration := fresh, duration := duration,
        explicitDeadline := deadline }
  | .recoverDropped boundary expected fresh duration deadline =>
      { kind := "recover_dropped", boundary := boundaryName boundary,
        expectedGeneration := expected, freshGeneration := fresh, duration := duration,
        explicitDeadline := deadline }
  | .finalize boundary generation outcome =>
      { kind := "finalize", boundary := boundaryName boundary, generation := generation,
        outcome := outcomeName outcome }
  | .policyRevoke boundary expected fresh outcome =>
      { kind := "policy_revoke", boundary := boundaryName boundary,
        expectedGeneration := expected, freshGeneration := fresh, outcome := outcomeName outcome }
  | .recoverExpiredAndFail boundary expected fresh =>
      { kind := "recover_expired_and_fail", boundary := boundaryName boundary,
        expectedGeneration := expected, freshGeneration := fresh }
  | .recoverExpiredTerminal boundary expected fresh outcome =>
      { kind := "recover_expired_terminal", boundary := boundaryName boundary,
        expectedGeneration := expected, freshGeneration := fresh, outcome := outcomeName outcome }
  | .recoverDroppedAndFail boundary expected fresh =>
      { kind := "recover_dropped_and_fail", boundary := boundaryName boundary,
        expectedGeneration := expected, freshGeneration := fresh }

def ActionWire.toModel (wire : ActionWire) : Action Generation :=
  let boundary := boundaryOf (wire.boundary.getD "")
  let generation := wire.generation.getD 0
  let duration := wire.duration.getD 0
  let deadline := wire.explicitDeadline.getD 0
  let expected := wire.expectedGeneration.getD 0
  let fresh := wire.freshGeneration.getD 0
  let outcome := outcomeOf (wire.outcome.getD "")
  match wire.kind with
  | "claim" => .claim boundary generation duration deadline
  | "park_backend" => .parkBackend boundary generation (wire.closed.getD false)
  | "resume_backend" => .resumeBackend boundary generation duration deadline (wire.wake.getD false)
  | "repark_expired_resume" =>
      .reparkExpiredResume boundary generation (wire.expectedDeadline.getD 0)
  | "finalize_parked" => .finalizeParked boundary generation outcome
  | "begin" => .begin boundary generation
  | "append_output" => .appendOutput boundary generation
  | "renew" => .renew boundary generation (wire.expectedDeadline.getD 0)
  | "authorize_producer_decision" =>
      .authorizeProducerDecision boundary generation (decisionOf (wire.decision.getD ""))
  | "socket_traffic" => .socketTraffic generation
  | "no_op" => .noOp generation
  | "drop" => .drop boundary generation
  | "recover_expired" => .recoverExpired boundary expected fresh duration deadline
  | "recover_dropped" => .recoverDropped boundary expected fresh duration deadline
  | "finalize" => .finalize boundary generation outcome
  | "policy_revoke" => .policyRevoke boundary expected fresh outcome
  | "recover_expired_and_fail" => .recoverExpiredAndFail boundary expected fresh
  | "recover_expired_terminal" => .recoverExpiredTerminal boundary expected fresh outcome
  | "recover_dropped_and_fail" => .recoverDroppedAndFail boundary expected fresh
  | _ => .advanceTime (wire.now.getD 0)

structure StepInput where
  name : String
  pre : WorldWire
  action : ActionWire
  deriving Lean.ToJson

def StepInput.ofCase (testCase : LeaseCase) : StepInput :=
  { name := testCase.name, pre := WorldWire.ofModel testCase.pre,
    action := ActionWire.ofModel testCase.action }

theorem leaseCases_wire_round_trip :
    leaseCases.all (fun testCase =>
      (WorldWire.ofModel testCase.pre).toModel == testCase.pre &&
        (ActionWire.ofModel testCase.action).toModel == testCase.action) = true := by
  native_decide

def stepOf (input : StepInput) : Option (World Generation) :=
  step? input.pre.toModel input.action.toModel

def leaseDeadline : Lease Generation → Option Nat
  | .active _ _ deadline | .recoverable _ _ deadline => some deadline
  | .vacant | .terminal _ _ => none

def leaseGeneration : Lease Generation → Option Nat
  | .active generation _ _ | .recoverable generation _ _ | .terminal generation _ => some generation
  | .vacant => none

/-- `authorize_begin`, `authorize_output_append` and `authorize_producer_decision`
decide on an active lease observed at the mutation gate; other leases and
boundaries have no native admission call. -/
def admissionCases : List LeaseCase :=
  leaseCases.filter fun testCase =>
    match testCase.pre.lease, testCase.action with
    | .active .., .begin .mutationWriteGate _
    | .active .., .appendOutput .mutationWriteGate _
    | .active .., .authorizeProducerDecision .mutationWriteGate _ _ => true
    | _, _ => false

@[conformance_family]
def admission : Family StepInput Bool where
  inputs := admissionCases.map StepInput.ofCase
  model := fun i => (stepOf i).isSome

/-- `authorize_renewal` decides on an active lease observed at the mutation gate. -/
def renewalCases : List LeaseCase :=
  leaseCases.filter fun testCase =>
    match testCase.pre.lease, testCase.action with
    | .active .., .renew .mutationWriteGate _ _ => true
    | _, _ => false

@[conformance_family]
def renewal : Family StepInput (Option Nat) where
  inputs := renewalCases.map StepInput.ofCase
  model := fun i => (stepOf i).bind fun world => leaseDeadline world.lease

/-- `relinquish_execution_lease` is the native drop at the mutation gate. -/
def relinquishmentCases : List LeaseCase :=
  leaseCases.filter fun testCase =>
    match testCase.action with
    | .drop .mutationWriteGate _ => true
    | _ => false

structure RelinquishOutput where
  request : RequestState
  generation : Option Nat
  status : String
  deriving Lean.ToJson

@[conformance_family]
def relinquishment : Family StepInput (Option RelinquishOutput) where
  inputs := relinquishmentCases.map StepInput.ofCase
  model := fun i => (stepOf i).map fun world =>
    { request := world.request, generation := leaseGeneration world.lease,
      status := (LeaseWire.ofModel world.lease).status }

/-- model-production-gap: no production owner computes the whole lease world
(used generations, effective expiry, continuation and token-charge counters);
native owners replay its admission, renewal and relinquishment projections in
`admission`, `renewal` and `relinquishment`, and claim, finalization,
recovery, revocation, observing-replica and clock steps have no native replay. -/
@[conformance_family]
def stepWorldUnreached : Family StepInput (Option WorldWire) where
  inputs := leaseCases.map StepInput.ofCase
  model := fun i => (stepOf i).map WorldWire.ofModel

example :
    (leaseCases.map fun testCase => (ActionWire.ofModel testCase.action).kind).eraseDups.length = 16 ∧
      ["advance_time", "append_output", "authorize_producer_decision", "begin", "claim", "drop",
        "finalize", "no_op", "policy_revoke", "recover_dropped", "recover_dropped_and_fail",
        "recover_expired", "recover_expired_and_fail", "recover_expired_terminal", "renew",
        "socket_traffic"].all (fun kind =>
          (leaseCases.map fun testCase => (ActionWire.ofModel testCase.action).kind).contains kind) := by
  native_decide

example :
    (admissionCases.map fun testCase => (ActionWire.ofModel testCase.action).kind).eraseDups =
        ["begin", "append_output", "authorize_producer_decision"] ∧
      renewalCases ≠ [] := by
  native_decide

def leaseTraceCaseJson (testCase : LeaseTraceCase) : String :=
  "{" ++ "\"name\":" ++ jsonString testCase.name ++
    ",\"pre\":" ++ worldJson testCase.pre ++
    ",\"actions\":" ++ jsonArray (testCase.actions.map actionJson) ++
    ",\"expected\":" ++ optionalWorldJson testCase.expected ++ "}"

def leaseTraceCasesJson : String := jsonArray (leaseTraceCases.map leaseTraceCaseJson)

structure ProviderEofCase where
  sawExplicitFinal : Bool
  expectedFailure : Bool
  deriving DecidableEq, Repr

def providerEofCases : List ProviderEofCase := [⟨false, true⟩, ⟨true, false⟩]

theorem providerEofCases_hold :
    providerEofCases.all (fun c =>
      providerEofIsFailure c.sawExplicitFinal == c.expectedFailure) = true := by
  native_decide

structure ProviderEofInput where
  sawExplicitFinal : Bool
  deriving Lean.ToJson

structure ProviderEofOutput where
  failure : Bool
  deriving Lean.ToJson

@[conformance_family]
def providerEof : Family ProviderEofInput ProviderEofOutput where
  inputs := providerEofCases.map fun c => { sawExplicitFinal := c.sawExplicitFinal }
  model := fun i => { failure := providerEofIsFailure i.sawExplicitFinal }


structure ObservedLeaseCase where
  name : String
  pre : World Generation
  action : Action Generation
  intervening : List (Action Generation)
  deriving DecidableEq, Repr

def observedLeaseCases : List ObservedLeaseCase :=
  [ ⟨"renewal_admission_after_expiry_is_rejected",
      processing 101 20 20 20, .renew .validatedTransaction 101 20, []⟩
  , ⟨"admitted_renewal_can_commit_after_clock_expiry_without_storage_conflict",
      processing 101 20 20 15, .renew .validatedTransaction 101 20, [.advanceTime 25]⟩
  , ⟨"recovery_invalidates_admitted_renewal",
      processing 101 20 20 15, .renew .validatedTransaction 101 20,
      [.advanceTime 25, .recoverExpiredAndFail .mutationWriteGate 101 202]⟩
  , ⟨"renewal_invalidates_admitted_publication",
      processing 101 20 20 15,
      .authorizeProducerDecision .mutationWriteGate 101 .acceptAndPublish,
      [.renew .validatedTransaction 101 20]⟩
  , ⟨"finalization_invalidates_admitted_renewal",
      processing 101 20 20 15, .renew .validatedTransaction 101 20,
      [.finalize .mutationWriteGate 101 .completed]⟩
  , ⟨"revocation_invalidates_admitted_renewal",
      processing 101 20 20 15, .renew .validatedTransaction 101 20,
      [.policyRevoke .mutationWriteGate 101 202 .dead]⟩
  , ⟨"drop_invalidates_admitted_renewal",
      processing 101 20 20 15, .renew .validatedTransaction 101 20,
      [.drop .mutationWriteGate 101]⟩
  , ⟨"competing_deadline_cas_invalidates_admitted_renewal",
      processing 101 20 20 15, .renew .validatedTransaction 101 20,
      [.renew .validatedTransaction 101 20]⟩
  ]

def observedLeaseExpected (testCase : ObservedLeaseCase) : Option (World Generation) := do
  let observation ← observeTransaction? testCase.pre testCase.action
  let current ← replay? testCase.pre testCase.intervening
  commitObservedTransaction? current observation

theorem clock_passage_is_not_storage_conflict :
    (observedLeaseCases[1]? >>= observedLeaseExpected) =
      some (processing 101 20 35 25) := by
  native_decide

theorem recovery_rejects_previously_admitted_renewal :
    (observedLeaseCases[2]? >>= observedLeaseExpected) = none := by
  native_decide

structure ObservedInput where
  name : String
  pre : WorldWire
  action : ActionWire
  intervening : List ActionWire
  deriving Lean.ToJson

def ObservedInput.ofCase (testCase : ObservedLeaseCase) : ObservedInput :=
  { name := testCase.name, pre := WorldWire.ofModel testCase.pre,
    action := ActionWire.ofModel testCase.action,
    intervening := testCase.intervening.map ActionWire.ofModel }

def ObservedInput.toCase (input : ObservedInput) : ObservedLeaseCase :=
  ⟨input.name, input.pre.toModel, input.action.toModel, input.intervening.map ActionWire.toModel⟩

theorem observedLeaseCases_wire_round_trip :
    observedLeaseCases.all (fun testCase =>
      (ObservedInput.ofCase testCase).toCase == testCase) = true := by
  native_decide

example : observedLeaseCases.length = 8 ∧ observedLeaseCases.all (·.name ≠ "") := by
  native_decide

def observedCasesNamed (names : List String) : List ObservedInput :=
  (observedLeaseCases.filter fun testCase => testCase.name ∈ names).map ObservedInput.ofCase

structure PublicationOutput where
  renewed : Bool
  committed : Bool
  deriving Lean.ToJson

/-- A served request holds one lease, so the publication scenario stages its
transaction against that lease and renews it once before committing. -/
@[conformance_family]
def observedPublication : Family ObservedInput PublicationOutput where
  inputs := observedCasesNamed ["renewal_invalidates_admitted_publication"]
  model := fun i =>
    { renewed := (replay? i.pre.toModel (i.intervening.map ActionWire.toModel)).isSome
      committed := (observedLeaseExpected i.toCase).isSome }

structure RecoveryOutput where
  staged : Bool
  recovered : Bool
  committed : Bool
  request : Option RequestState
  generation : Option Nat
  deriving Lean.ToJson

/-- A served request holds one lease, so the recovery scenario stages one
renewal transaction and recovers that lease underneath it; its fresh
generation is stored as a fixture label naming the modeled generation. -/
@[conformance_family]
def observedRecovery : Family ObservedInput RecoveryOutput where
  inputs := observedCasesNamed ["recovery_invalidates_admitted_renewal"]
  model := fun i =>
    let current := replay? i.pre.toModel (i.intervening.map ActionWire.toModel)
    { staged := (observeTransaction? i.pre.toModel i.action.toModel).isSome
      recovered := current.isSome
      committed := (observedLeaseExpected i.toCase).isSome
      request := current.map (·.request)
      generation := current.bind fun world => leaseGeneration world.lease }

structure RecheckInput where
  admitted : ObservedInput
  recheck : ObservedInput
  deriving Lean.ToJson

/-- `renew_with_recheck_at` admits a renewal at one instant and rechecks it at a
later one on the same served request, so the scenario pairs the renewal admitted
before expiry with the renewal admitted at expiry. -/
@[conformance_family]
def observedRecheck : Family RecheckInput Bool where
  inputs :=
    (observedCasesNamed ["admitted_renewal_can_commit_after_clock_expiry_without_storage_conflict"]).flatMap
      fun admitted => (observedCasesNamed ["renewal_admission_after_expiry_is_rejected"]).map
        fun recheck => { admitted, recheck }
  model := fun i => (observedLeaseExpected i.recheck.toCase).isSome

/-- model-production-gap: no production owner computes the whole lease world
after an observed transaction commits; the served scenarios replay their commit
outcomes in `observedPublication`, `observedRecovery` and `observedRecheck`, and
finalization, revocation, drop and competing-renewal interleavings have no
served scenario. -/
@[conformance_family]
def observedWorldUnreached : Family ObservedInput (Option WorldWire) where
  inputs := observedLeaseCases.map ObservedInput.ofCase
  model := fun i => (observedLeaseExpected i.toCase).map WorldWire.ofModel

end Conformance.RequestExecutionLeaseContracts
