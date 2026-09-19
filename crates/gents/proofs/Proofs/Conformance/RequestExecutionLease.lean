import Proofs.RequestExecutionLease
import Proofs.Conformance.ContractTypes

namespace Conformance.RequestExecutionLeaseContracts

open Conformance.Contracts
open RequestExecutionLease

abbrev Generation := Nat

private def fact (id generation createdAt : Nat)
    (eligibility : OutputEligibility := .currentRequest) : OutputFact Generation :=
  ⟨id, generation, createdAt, eligibility⟩

private def world
    (request : RequestState) (lease : Lease Generation)
    (usedGenerations : List Generation) (output : List (OutputFact Generation))
    (now : Nat)
    (continuationRequired tokenChargeRequired : Bool := true)
    (continuationCount tokenChargeCount : Nat := 0) : World Generation :=
  { request
  , lease
  , usedGenerations
  , output
  , now
  , continuationRequired
  , tokenChargeRequired
  , continuationCount
  , tokenChargeCount
  }

private def vacant : World Generation :=
  world .pending .vacant [] [] 0

private def claimed
    (generation duration deadline now : Nat := 1) : World Generation :=
  world .claimed (.active generation duration deadline) [generation] [] now

private def processing
    (generation duration deadline now : Nat := 1)
    (output : List (OutputFact Generation) := []) : World Generation :=
  world .processing (.active generation duration deadline) [generation]
    output now

private def recoverable
    (generation duration deadline now : Nat := 1)
    (output : List (OutputFact Generation) := []) : World Generation :=
  world .processing (.recoverable generation duration deadline) [generation]
    output now

private def recovered
    (oldGeneration generation duration deadline now : Nat)
    (output : List (OutputFact Generation) := []) : World Generation :=
  world .processing (.active generation duration deadline)
    [generation, oldGeneration] output now

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
  , { name := "raw_output_append_stamps_owner_clock_without_deadline_rewrite"
    , pre := processing 101 5 10 4
    , action := .appendOutput .mutationWriteGate 101 501 .currentRequest
    , expected := some (processing 101 5 10 4 [fact 501 101 4])
    }
  , { name := "raw_output_rejects_identity_collision"
    , pre := processing 101 5 10 4 [fact 501 101 3]
    , action := .appendOutput .mutationWriteGate 101 501 .currentRequest
    , expected := none
    }
  , { name := "exact_replay_is_identity_even_after_expiry"
    , pre := processing 101 5 10 12 [fact 501 101 3]
    , action := .replayOutput (fact 501 101 3)
    , expected := some (processing 101 5 10 12 [fact 501 101 3])
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
    , action := .appendOutput .mutationWriteGate 101 501 .currentRequest
    , expected := none
    }
  , { name := "stale_generation_cannot_renew"
    , pre := { processing 202 5 20 5 with usedGenerations := [202, 101] }
    , action := .renew .mutationWriteGate 101
    , expected := none
    }
  , { name := "stale_generation_cannot_finalize"
    , pre := { processing 202 5 20 5 with usedGenerations := [202, 101] }
    , action := .finalize .mutationWriteGate 101 .completed
    , expected := none
    }
  , { name := "deadline_boundary_rejects_raw_append"
    , pre := processing 101 5 10 10
    , action := .appendOutput .mutationWriteGate 101 501 .currentRequest
    , expected := none
    }
  , { name := "deadline_boundary_rejects_explicit_renewal"
    , pre := processing 101 5 10 10
    , action := .renew .mutationWriteGate 101
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
  , { name := "recent_eligible_output_prevents_atomic_expiry_recovery"
    , pre := processing 101 5 10 11 [fact 501 101 9]
    , action := .recoverExpired .mutationWriteGate 101 202 5 30
    , expected := none
    }
  , { name := "foreign_request_output_does_not_prevent_expiry_recovery"
    , pre := processing 101 5 10 11 [fact 501 101 9 .foreignRequest]
    , action := .recoverExpired .mutationWriteGate 101 202 5 30
    , expected := some
        (recovered 101 202 5 30 11 [fact 501 101 9 .foreignRequest])
    }
  , { name := "fork_output_does_not_prevent_expiry_recovery"
    , pre := processing 101 5 10 11 [fact 501 101 9 .fork]
    , action := .recoverExpired .mutationWriteGate 101 202 5 30
    , expected := some (recovered 101 202 5 30 11 [fact 501 101 9 .fork])
    }
  , { name := "tool_owned_output_does_not_prevent_request_recovery"
    , pre := processing 101 5 10 11 [fact 501 101 9 .toolOwned]
    , action := .recoverExpired .mutationWriteGate 101 202 5 30
    , expected := some (recovered 101 202 5 30 11 [fact 501 101 9 .toolOwned])
    }
  , { name := "beyond_extent_output_does_not_prevent_expiry_recovery"
    , pre := processing 101 5 10 11 [fact 501 101 9 .beyondExtent]
    , action := .recoverExpired .mutationWriteGate 101 202 5 30
    , expected := some (recovered 101 202 5 30 11 [fact 501 101 9 .beyondExtent])
    }
  , { name := "malformed_output_does_not_prevent_expiry_recovery"
    , pre := processing 101 5 10 11 [fact 501 101 9 .malformed]
    , action := .recoverExpired .mutationWriteGate 101 202 5 30
    , expected := some (recovered 101 202 5 30 11 [fact 501 101 9 .malformed])
    }
  , { name := "current_request_conflict_is_integrity_failure_not_inactivity"
    , pre := processing 101 5 10 11 [fact 501 101 9 .currentRequestConflict]
    , action := .recoverExpired .mutationWriteGate 101 202 5 30
    , expected := none
    }
  , { name := "observing_replica_cannot_recover_expired_work"
    , pre := processing 101 5 10 11
    , action := .recoverExpired .observingReplica 101 202 5 30
    , expected := none
    }
  , { name := "observing_replica_cannot_append"
    , pre := processing 101 5 10 5
    , action := .appendOutput .observingReplica 101 501 .currentRequest
    , expected := none
    }
  , { name := "silent_renewal_extends_from_owner_clock"
    , pre := processing 101 5 10 8
    , action := .renew .mutationWriteGate 101
    , expected := some (processing 101 5 13 8)
    }
  , { name := "renewal_advances_previous_deadline_on_tie"
    , pre := processing 101 5 10 5
    , action := .renew .mutationWriteGate 101
    , expected := some (processing 101 5 11 5)
    }
  , { name := "close_or_retract_authorization_renews_without_claiming_closure"
    , pre := processing 101 5 10 5
    , action := .authorizeProducerDecision .mutationWriteGate 101 .closeOrRetract
    , expected := some (processing 101 5 11 5)
    }
  , { name := "accept_and_publish_authorization_renews_without_modeling_header"
    , pre := processing 101 5 10 5
    , action := .authorizeProducerDecision .mutationWriteGate 101 .acceptAndPublish
    , expected := some (processing 101 5 11 5)
    }
  , { name := "dispatch_authorization_renews_without_modeling_tool_rows"
    , pre := processing 101 5 10 5
    , action := .authorizeProducerDecision .mutationWriteGate 101 .dispatch
    , expected := some (processing 101 5 11 5)
    }
  , { name := "drop_relinquishes_for_distinct_recovery_path"
    , pre := processing 101 5 10 5
    , action := .drop .mutationWriteGate 101
    , expected := some (recoverable 101 5 10 5)
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
        (world .failed (.terminal 202 .failed) [202, 101] [] 11 true true 1 1)
    }
  , { name := "dropped_recovery_failure_atomically_elects_terminal_winner"
    , pre := recoverable 101 5 10 11
    , action := .recoverDroppedAndFail .mutationWriteGate 101 202
    , expected := some
        (world .failed (.terminal 202 .failed) [202, 101] [] 11 true true 1 1)
    }
  , { name := "completion_atomically_terminalizes_request"
    , pre := processing 101 5 10 5
    , action := .finalize .mutationWriteGate 101 .completed
    , expected := some
        (world .completed (.terminal 101 .completed) [101] [] 5 true true 1 1)
    }
  , { name := "provider_eof_fails_claimed_request_atomically"
    , pre := claimed 101 5 10 5
    , action := .finalize .mutationWriteGate 101 .failed
    , expected := some
        (world .failed (.terminal 101 .failed) [101] [] 5 true true 1 1)
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
        (world .superseded (.terminal 202 .superseded) [202, 101] [] 5 true true 1 1)
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
  , { name := "future_foreign_fact_does_not_block_current_request_recovery"
    , pre := processing 101 5 10 11 [fact 501 999 100 .foreignRequest]
    , action := .recoverExpired .mutationWriteGate 101 202 5 30
    , expected := some
        (recovered 101 202 5 30 11 [fact 501 999 100 .foreignRequest])
    }
  , { name := "clock_cannot_move_backwards"
    , pre := processing 101 5 10 5
    , action := .advanceTime 4
    , expected := none
    }
  ]

theorem leaseCases_count : leaseCases.length = 50 := by native_decide

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
        (world .failed (.terminal 202 .failed) [202, 101] [] 10 true true 1 1)
    }
  , { name := "eligible_output_extends_liveness_without_request_rewrite"
    , pre := processing 101 5 10 9
    , actions :=
        [ .appendOutput .mutationWriteGate 101 501 .currentRequest
        , .advanceTime 11
        , .finalize .mutationWriteGate 101 .completed
        ]
    , expected := some
        (world .completed (.terminal 101 .completed) [101] [fact 501 101 9]
          11 true true 1 1)
    }
  , { name := "exact_replay_does_not_revive_expired_generation"
    , pre := processing 101 5 10 9 [fact 501 101 4]
    , actions :=
        [ .advanceTime 10
        , .replayOutput (fact 501 101 4)
        , .renew .mutationWriteGate 101
        ]
    , expected := none
    }
  , { name := "silent_work_renews_then_completes"
    , pre := processing 101 5 10 8
    , actions :=
        [ .renew .mutationWriteGate 101
        , .advanceTime 12
        , .finalize .mutationWriteGate 101 .completed
        ]
    , expected := some
        (world .completed (.terminal 101 .completed) [101] [] 12 true true 1 1)
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
        , .appendOutput .mutationWriteGate 101 501 .currentRequest
        , .authorizeProducerDecision .mutationWriteGate 101 .acceptAndPublish
        ]
    , expected := some (processing 101 5 12 5 [fact 501 101 5])
    }
  ]

theorem leaseTraceCases_count : leaseTraceCases.length = 6 := by native_decide

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
  | .observingReplica => "observing_replica"

def eligibilityName : OutputEligibility → String
  | .currentRequest => "current_request"
  | .foreignRequest => "foreign_request"
  | .fork => "fork"
  | .toolOwned => "tool_owned"
  | .beyondExtent => "beyond_extent"
  | .malformed => "malformed"
  | .currentRequestConflict => "current_request_conflict"

def decisionName : ProducerDecision → String
  | .closeOrRetract => "close_or_retract"
  | .acceptAndPublish => "accept_and_publish"
  | .dispatch => "dispatch"

def outputFactJson (value : OutputFact Generation) : String :=
  "{" ++ "\"id\":" ++ toString value.id ++
    ",\"generation\":" ++ toString value.generation ++
    ",\"created_at\":" ++ toString value.createdAt ++
    ",\"eligibility\":" ++ jsonString (eligibilityName value.eligibility) ++ "}"

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
    ++ "\"output\":" ++ jsonArray (value.output.map outputFactJson) ++ ","
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
  | .claim boundary generation duration deadline =>
      "{\"kind\":\"claim\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++
        ",\"duration\":" ++ toString duration ++
        ",\"explicit_deadline\":" ++ toString deadline ++ "}"
  | .begin boundary generation =>
      "{\"kind\":\"begin\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ "}"
  | .appendOutput boundary generation id eligibility =>
      "{\"kind\":\"append_output\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ ",\"id\":" ++ toString id ++
        ",\"eligibility\":" ++ jsonString (eligibilityName eligibility) ++ "}"
  | .replayOutput output =>
      "{\"kind\":\"replay_output\",\"fact\":" ++ outputFactJson output ++ "}"
  | .renew boundary generation =>
      "{\"kind\":\"renew\",\"boundary\":" ++ jsonString (boundaryName boundary) ++
        ",\"generation\":" ++ toString generation ++ "}"
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

def providerEofCasesJson : String :=
  jsonArray (providerEofCases.map (fun c =>
    "{\"saw_explicit_final\":" ++ boolJson c.sawExplicitFinal ++
      ",\"expected_failure\":" ++ boolJson c.expectedFailure ++ "}"))

end Conformance.RequestExecutionLeaseContracts
