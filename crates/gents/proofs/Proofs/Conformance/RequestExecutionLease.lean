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

def leaseCaseJson (testCase : LeaseCase) : String :=
  "{" ++ "\"name\":" ++ jsonString testCase.name ++
    ",\"pre\":" ++ worldJson testCase.pre ++
    ",\"action\":" ++ actionJson testCase.action ++
    ",\"expected\":" ++ optionalWorldJson testCase.expected ++ "}"

def leaseCasesJson : String := jsonArray (leaseCases.map leaseCaseJson)

structure ProviderEofCase where
  sawExplicitFinal : Bool
  expectedFailure : Bool
  deriving DecidableEq, Repr

def providerEofCases : List ProviderEofCase := [⟨false, true⟩, ⟨true, false⟩]

theorem providerEofCases_hold :
    providerEofCases.all (fun c =>
      providerEofIsFailure c.sawExplicitFinal == c.expectedFailure) = true := by
  native_decide

def providerEofCasesJson : String :=
  jsonArray (providerEofCases.map (fun c =>
    "{\"saw_explicit_final\":" ++ boolJson c.sawExplicitFinal ++
      ",\"expected_failure\":" ++ boolJson c.expectedFailure ++ "}"))


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

def observedLeaseCaseJson (testCase : ObservedLeaseCase) : String :=
  "{" ++ "\"name\":" ++ jsonString testCase.name ++
    ",\"pre\":" ++ worldJson testCase.pre ++
    ",\"action\":" ++ actionJson testCase.action ++
    ",\"intervening\":" ++ jsonArray (testCase.intervening.map actionJson) ++
    ",\"expected\":" ++ optionalWorldJson (observedLeaseExpected testCase) ++ "}"

def observedLeaseCasesJson : String := jsonArray (observedLeaseCases.map observedLeaseCaseJson)

deriving instance Lean.ToJson for Boundary, ProducerDecision

inductive LeaseStatus where
  | vacant
  | active
  | recoverable
  | terminal
  deriving Lean.ToJson

structure LeaseWire where
  status : LeaseStatus
  generation : Option Nat
  duration : Option Nat
  explicitDeadline : Option Nat
  outcome : Option Outcome
  deriving Lean.ToJson

def LeaseWire.ofModel : Lease Generation → LeaseWire
  | .vacant => ⟨.vacant, none, none, none, none⟩
  | .active generation duration deadline =>
    ⟨.active, some generation, some duration, some deadline, none⟩
  | .recoverable generation duration deadline =>
    ⟨.recoverable, some generation, some duration, some deadline, none⟩
  | .terminal generation outcome => ⟨.terminal, some generation, none, none, some outcome⟩

def LeaseWire.toModel (lease : LeaseWire) : Lease Generation :=
  match lease.status with
  | .vacant => .vacant
  | .active =>
    .active (lease.generation.getD 0) (lease.duration.getD 0) (lease.explicitDeadline.getD 0)
  | .recoverable =>
    .recoverable (lease.generation.getD 0) (lease.duration.getD 0)
      (lease.explicitDeadline.getD 0)
  | .terminal => .terminal (lease.generation.getD 0) (lease.outcome.getD .failed)

/-- `effectiveExpiry` is derived from the lease and ignored when a wire world is
read back as a model world. -/
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

def WorldWire.ofModel (value : World Generation) : WorldWire :=
  { request := value.request, lease := .ofModel value.lease
    usedGenerations := value.usedGenerations, now := value.now
    effectiveExpiry := RequestExecutionLease.effectiveExpiry value
    continuationRequired := value.continuationRequired
    tokenChargeRequired := value.tokenChargeRequired
    continuationCount := value.continuationCount, tokenChargeCount := value.tokenChargeCount }

def WorldWire.toModel (value : WorldWire) : World Generation :=
  { request := value.request, lease := value.lease.toModel
    usedGenerations := value.usedGenerations, now := value.now
    continuationRequired := value.continuationRequired
    tokenChargeRequired := value.tokenChargeRequired
    continuationCount := value.continuationCount, tokenChargeCount := value.tokenChargeCount }

inductive ActionKind where
  | claim
  | parkBackend
  | resumeBackend
  | reparkExpiredResume
  | finalizeParked
  | begin
  | appendOutput
  | renew
  | authorizeProducerDecision
  | socketTraffic
  | noOp
  | advanceTime
  | drop
  | recoverExpired
  | recoverDropped
  | finalize
  | policyRevoke
  | recoverExpiredAndFail
  | recoverExpiredTerminal
  | recoverDroppedAndFail
  deriving Lean.ToJson

/-- Each action carries only the parameters its constructor takes; the others
are absent. -/
structure ActionWire where
  kind : ActionKind
  boundary : Option Boundary := none
  generation : Option Nat := none
  expectedGeneration : Option Nat := none
  freshGeneration : Option Nat := none
  duration : Option Nat := none
  explicitDeadline : Option Nat := none
  expectedDeadline : Option Nat := none
  now : Option Nat := none
  outcome : Option Outcome := none
  decision : Option ProducerDecision := none
  closed : Option Bool := none
  wake : Option Bool := none
  deriving Lean.ToJson

def ActionWire.ofModel : Action Generation → ActionWire
  | .claim b g d e =>
    { kind := .claim, boundary := some b, generation := some g, duration := some d
      explicitDeadline := some e }
  | .parkBackend b g closed =>
    { kind := .parkBackend, boundary := some b, generation := some g, closed := some closed }
  | .resumeBackend b g d e wake =>
    { kind := .resumeBackend, boundary := some b, generation := some g, duration := some d
      explicitDeadline := some e, wake := some wake }
  | .reparkExpiredResume b g e =>
    { kind := .reparkExpiredResume, boundary := some b, generation := some g
      expectedDeadline := some e }
  | .finalizeParked b g o =>
    { kind := .finalizeParked, boundary := some b, generation := some g, outcome := some o }
  | .begin b g => { kind := .begin, boundary := some b, generation := some g }
  | .appendOutput b g => { kind := .appendOutput, boundary := some b, generation := some g }
  | .renew b g e =>
    { kind := .renew, boundary := some b, generation := some g, expectedDeadline := some e }
  | .authorizeProducerDecision b g decision =>
    { kind := .authorizeProducerDecision, boundary := some b, generation := some g
      decision := some decision }
  | .socketTraffic g => { kind := .socketTraffic, generation := some g }
  | .noOp g => { kind := .noOp, generation := some g }
  | .advanceTime now => { kind := .advanceTime, now := some now }
  | .drop b g => { kind := .drop, boundary := some b, generation := some g }
  | .recoverExpired b expected fresh d e =>
    { kind := .recoverExpired, boundary := some b, expectedGeneration := some expected
      freshGeneration := some fresh, duration := some d, explicitDeadline := some e }
  | .recoverDropped b expected fresh d e =>
    { kind := .recoverDropped, boundary := some b, expectedGeneration := some expected
      freshGeneration := some fresh, duration := some d, explicitDeadline := some e }
  | .finalize b g o =>
    { kind := .finalize, boundary := some b, generation := some g, outcome := some o }
  | .policyRevoke b expected fresh o =>
    { kind := .policyRevoke, boundary := some b, expectedGeneration := some expected
      freshGeneration := some fresh, outcome := some o }
  | .recoverExpiredAndFail b expected fresh =>
    { kind := .recoverExpiredAndFail, boundary := some b, expectedGeneration := some expected
      freshGeneration := some fresh }
  | .recoverExpiredTerminal b expected fresh o =>
    { kind := .recoverExpiredTerminal, boundary := some b, expectedGeneration := some expected
      freshGeneration := some fresh, outcome := some o }
  | .recoverDroppedAndFail b expected fresh =>
    { kind := .recoverDroppedAndFail, boundary := some b, expectedGeneration := some expected
      freshGeneration := some fresh }

def ActionWire.toModel (a : ActionWire) : Action Generation :=
  let b := a.boundary.getD .observingReplica
  let g := a.generation.getD 0
  let expected := a.expectedGeneration.getD 0
  let fresh := a.freshGeneration.getD 0
  let d := a.duration.getD 0
  let e := a.explicitDeadline.getD 0
  let o := a.outcome.getD .failed
  match a.kind with
  | .claim => .claim b g d e
  | .parkBackend => .parkBackend b g (a.closed.getD false)
  | .resumeBackend => .resumeBackend b g d e (a.wake.getD false)
  | .reparkExpiredResume => .reparkExpiredResume b g (a.expectedDeadline.getD 0)
  | .finalizeParked => .finalizeParked b g o
  | .begin => .begin b g
  | .appendOutput => .appendOutput b g
  | .renew => .renew b g (a.expectedDeadline.getD 0)
  | .authorizeProducerDecision =>
    .authorizeProducerDecision b g (a.decision.getD .closeOrRetract)
  | .socketTraffic => .socketTraffic g
  | .noOp => .noOp g
  | .advanceTime => .advanceTime (a.now.getD 0)
  | .drop => .drop b g
  | .recoverExpired => .recoverExpired b expected fresh d e
  | .recoverDropped => .recoverDropped b expected fresh d e
  | .finalize => .finalize b g o
  | .policyRevoke => .policyRevoke b expected fresh o
  | .recoverExpiredAndFail => .recoverExpiredAndFail b expected fresh
  | .recoverExpiredTerminal => .recoverExpiredTerminal b expected fresh o
  | .recoverDroppedAndFail => .recoverDroppedAndFail b expected fresh

example : leaseTraceCases.all (fun testCase =>
    (testCase.actions.map ActionWire.ofModel).map ActionWire.toModel == testCase.actions &&
      (WorldWire.ofModel testCase.pre).toModel == testCase.pre) = true := by
  native_decide

structure LeaseTraceInput where
  name : String
  pre : WorldWire
  actions : List ActionWire
  deriving Lean.ToJson

/-- model-production-gap: the native lease owners take one database-backed
action at a time under the wall clock (the generated drop cases drive
`relinquish_execution_lease`); no production path replays a multi-action trace
over a logical clock, so these traces of renewal, expiry, recovery and stale
owners are not reached. -/
@[conformance_family]
def leaseTraceUnreached : Conformance.Family LeaseTraceInput (Option WorldWire) where
  inputs := leaseTraceCases.map fun testCase =>
    { name := testCase.name, pre := .ofModel testCase.pre
      actions := testCase.actions.map ActionWire.ofModel }
  model := fun i =>
    (replay? i.pre.toModel (i.actions.map ActionWire.toModel)).map WorldWire.ofModel

end Conformance.RequestExecutionLeaseContracts
