import Proofs.RequestExecutionLease
import Proofs.Conformance.ContractTypes

namespace Conformance.RequestExecutionLeaseContracts

open Conformance.Contracts
open RequestExecutionLease

abbrev Generation := Nat

private def world
    (request : RequestPhase) (response : ResponsePhase)
    (lease : Lease Generation) (usedGenerations : List Generation)
    (now progressSeq : Nat)
    (continuationRequired tokenChargeRequired : Bool := true)
    (continuationCount tokenChargeCount : Nat := 0) : World Generation :=
  { request
  , response
  , lease
  , usedGenerations
  , now
  , progressSeq
  , continuationRequired
  , tokenChargeRequired
  , continuationCount
  , tokenChargeCount
  }

private def vacant : World Generation :=
  world .pending .absent .vacant [] 0 0

private def claimed (generation deadline now : Nat := 1) : World Generation :=
  world .claimed .absent (.active generation deadline) [generation] now 0

private def processing
    (generation deadline now : Nat := 1) (progressSeq : Nat := 0) : World Generation :=
  world .processing .streaming (.active generation deadline)
    [generation] now progressSeq

private def recoverable
    (generation now : Nat := 1) (progressSeq : Nat := 0) : World Generation :=
  world .processing .streaming (.recoverable generation)
    [generation] now progressSeq

structure LeaseCase where
  name : String
  pre : World Generation
  action : Action Generation
  expected : Option (World Generation)
  deriving DecidableEq, Repr

def leaseCases : List LeaseCase :=
  [ { name := "fresh_claim_installs_generation_and_deadline"
    , pre := vacant
    , action := .claim 101 10
    , expected := some
        (world .claimed .absent (.active 101 10) [101] 0 0)
    }
  , { name := "claim_rejects_reused_generation"
    , pre := { vacant with usedGenerations := [101] }
    , action := .claim 101 10
    , expected := none
    }
  , { name := "matching_owner_begins_streaming"
    , pre := claimed 101 10 1
    , action := .begin 101
    , expected := some (processing 101 10 1)
    }
  , { name := "response_progress_advances_and_renews"
    , pre := processing 101 10 5 7
    , action := .persistProgress 101 .response 20
    , expected := some (processing 101 20 5 8)
    }
  , { name := "tool_progress_advances_and_renews"
    , pre := processing 101 10 5 7
    , action := .persistProgress 101 .tool 20
    , expected := some (processing 101 20 5 8)
    }
  , { name := "transcript_progress_advances_and_renews"
    , pre := processing 101 10 5 7
    , action := .persistProgress 101 .transcript 20
    , expected := some (processing 101 20 5 8)
    }
  , { name := "socket_traffic_is_not_progress"
    , pre := processing 101 10 12 7
    , action := .socketTraffic 101
    , expected := some (processing 101 10 12 7)
    }
  , { name := "no_op_is_not_progress"
    , pre := processing 101 10 12 7
    , action := .noOp 101
    , expected := some (processing 101 10 12 7)
    }
  , { name := "stale_generation_cannot_renew"
    , pre := { processing 202 20 5 8 with usedGenerations := [202, 101] }
    , action := .persistProgress 101 .response 30
    , expected := none
    }
  , { name := "stale_generation_cannot_finalize"
    , pre := { processing 202 20 5 8 with usedGenerations := [202, 101] }
    , action := .finalize 101 .completed
    , expected := none
    }
  , { name := "expired_generation_cannot_finalize"
    , pre := processing 101 10 11 7
    , action := .finalize 101 .completed
    , expected := none
    }
  , { name := "expiry_relinquishes_for_recovery"
    , pre := processing 101 10 11 7
    , action := .expire 101
    , expected := some (recoverable 101 11 7)
    }
  , { name := "drop_relinquishes_for_recovery"
    , pre := processing 101 10 5 7
    , action := .drop 101
    , expected := some (recoverable 101 5 7)
    }
  , { name := "recovery_takes_fresh_generation"
    , pre := recoverable 101 11 7
    , action := .recover 101 202 30
    , expected := some
        { processing 202 30 11 7 with usedGenerations := [202, 101] }
    }
  , { name := "recovery_rejects_wrong_expected_generation"
    , pre := recoverable 101 11 7
    , action := .recover 999 202 30
    , expected := none
    }
  , { name := "recovery_rejects_aba_generation_reuse"
    , pre :=
        { recoverable 202 21 9 with usedGenerations := [202, 101] }
    , action := .recover 202 101 30
    , expected := none
    }
  , { name := "recovery_failure_is_atomic_and_single_effect"
    , pre := recoverable 101 11 7
    , action := .recoverAndFail 101 202
    , expected := some
        (world .failed .failed (.terminal 202 .failed) [202, 101]
          11 7 true true 1 1)
    }
  , { name := "completion_atomically_agrees_request_response"
    , pre := processing 101 10 5 7
    , action := .finalize 101 .completed
    , expected := some
        (world .completed .completed (.terminal 101 .completed) [101]
          5 7 true true 1 1)
    }
  , { name := "provider_eof_fails_claimed_pair_atomically"
    , pre := claimed 101 10 5
    , action := .finalize 101 .failed
    , expected := some
        (world .failed .failed (.terminal 101 .failed) [101]
          5 0 true true 1 1)
    }
  , { name := "interrupt_atomically_agrees_request_response"
    , pre := processing 101 10 5 7
    , action := .finalize 101 .interrupted
    , expected := some
        (world .interrupted .interrupted (.terminal 101 .interrupted) [101]
          5 7 true true 1 1)
    }
  , { name := "terminal_winner_rejects_second_finalize"
    , pre := world .completed .completed (.terminal 101 .completed) [101]
        5 7 true true 1 1
    , action := .finalize 101 .failed
    , expected := none
    }
  , { name := "terminal_winner_rejects_recovery_racer"
    , pre := world .failed .failed (.terminal 202 .failed) [202, 101]
        11 7 true true 1 1
    , action := .recoverAndFail 101 303
    , expected := none
    }
  , { name := "expired_generation_cannot_begin"
    , pre := claimed 101 10 11
    , action := .begin 101
    , expected := none
    }
  , { name := "expired_generation_cannot_renew"
    , pre := processing 101 10 11 7
    , action := .persistProgress 101 .response 20
    , expected := none
    }
  , { name := "completion_rejects_claimed_without_response"
    , pre := claimed 101 10 5
    , action := .finalize 101 .completed
    , expected := none
    }
  , { name := "renewal_must_extend_deadline"
    , pre := processing 101 10 5 7
    , action := .persistProgress 101 .response 10
    , expected := none
    }
  , { name := "deadline_boundary_allows_completion"
    , pre := processing 101 10 10 7
    , action := .finalize 101 .completed
    , expected := some (world .completed .completed (.terminal 101 .completed) [101] 10 7 true true 1 1)
    }
  , { name := "live_generation_can_be_superseded"
    , pre := processing 101 10 5 7
    , action := .revoke 101 10 7 202 .superseded
    , expected := some (world .superseded .failed (.terminal 202 .superseded) [202, 101] 5 7 true true 1 1)
    }
  , { name := "claimed_generation_can_be_declared_dead"
    , pre := claimed 101 10 5
    , action := .revoke 101 10 0 202 .dead
    , expected := some (world .dead .failed (.terminal 202 .dead) [202, 101] 5 0 true true 1 1)
    }
  , { name := "revocation_rejects_stale_generation"
    , pre := processing 101 10 5 7
    , action := .revoke 999 10 7 202 .dead
    , expected := none
    }
  , { name := "revocation_rejects_stale_expiry"
    , pre := processing 101 10 5 7
    , action := .revoke 101 9 7 202 .dead
    , expected := none
    }
  , { name := "revocation_rejects_stale_progress"
    , pre := processing 101 10 5 7
    , action := .revoke 101 10 6 202 .dead
    , expected := none
    }
  , { name := "revocation_rejects_reused_generation"
    , pre := processing 101 10 5 7
    , action := .revoke 101 10 7 101 .dead
    , expected := none
    }
  , { name := "revocation_cannot_claim_success"
    , pre := processing 101 10 5 7
    , action := .revoke 101 10 7 202 .completed
    , expected := none
    }
  ]

theorem leaseCases_count : leaseCases.length = 34 := by native_decide

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
  [ { name := "socket_and_noop_traffic_cannot_prevent_expiry_recovery"
    , pre := processing 101 10 9 7
    , actions :=
        [ .socketTraffic 101
        , .noOp 101
        , .advanceTime 11
        , .socketTraffic 101
        , .expire 101
        , .recoverAndFail 101 202
        ]
    , expected := some
        (world .failed .failed (.terminal 202 .failed) [202, 101]
          11 7 true true 1 1)
    }
  , { name := "semantic_progress_renews_before_success"
    , pre := processing 101 10 9 7
    , actions :=
        [ .persistProgress 101 .response 20
        , .advanceTime 11
        , .finalize 101 .completed
        ]
    , expected := some
        (world .completed .completed (.terminal 101 .completed) [101]
          11 8 true true 1 1)
    }
  , { name := "dropped_owner_recovers_and_fails_atomically"
    , pre := processing 101 10 5 7
    , actions := [.drop 101, .recoverAndFail 101 202]
    , expected := some
        (world .failed .failed (.terminal 202 .failed) [202, 101]
          5 7 true true 1 1)
    }
  , { name := "recovered_owner_wins_and_stale_owner_cannot_finalize"
    , pre := recoverable 101 11 7
    , actions := [.recover 101 202 30, .finalize 101 .completed]
    , expected := none
    }
  , { name := "one_terminal_winner_prevents_duplicate_effects"
    , pre := processing 101 10 5 7
    , actions := [.finalize 101 .completed, .finalize 101 .failed]
    , expected := none
    }
  ]

theorem leaseTraceCases_count : leaseTraceCases.length = 5 := by native_decide

theorem leaseTraceCases_hold :
    leaseTraceCases.all (fun testCase =>
      replay? testCase.pre testCase.actions == testCase.expected) = true := by
  native_decide

private def boolJson (value : Bool) : String :=
  if value then "true" else "false"

def requestPhaseName : RequestPhase → String
  | .pending => "pending"
  | .claimed => "claimed"
  | .processing => "processing"
  | .completed => "completed"
  | .failed => "failed"
  | .interrupted => "interrupted"
  | .dead => "dead"
  | .superseded => "superseded"

def responsePhaseName : ResponsePhase → String
  | .absent => "absent"
  | .streaming => "streaming"
  | .completed => "completed"
  | .failed => "failed"
  | .interrupted => "interrupted"

def outcomeName : Outcome → String
  | .completed => "completed"
  | .failed => "failed"
  | .interrupted => "interrupted"
  | .dead => "dead"
  | .superseded => "superseded"

def progressKindName : ProgressKind → String
  | .response => "response"
  | .tool => "tool"
  | .transcript => "transcript"

def leaseJson : Lease Generation → String
  | .vacant =>
      "{\"status\":\"vacant\",\"generation\":null,\"deadline\":null,\"outcome\":null}"
  | .active generation deadline =>
      "{\"status\":\"active\",\"generation\":" ++ toString generation ++
        ",\"deadline\":" ++ toString deadline ++ ",\"outcome\":null}"
  | .recoverable generation =>
      "{\"status\":\"recoverable\",\"generation\":" ++ toString generation ++
        ",\"deadline\":null,\"outcome\":null}"
  | .terminal generation outcome =>
      "{\"status\":\"terminal\",\"generation\":" ++ toString generation ++
        ",\"deadline\":null,\"outcome\":" ++ jsonString (outcomeName outcome) ++ "}"

def worldJson (value : World Generation) : String :=
  "{"
    ++ "\"request\":" ++ jsonString (requestPhaseName value.request) ++ ","
    ++ "\"response\":" ++ jsonString (responsePhaseName value.response) ++ ","
    ++ "\"lease\":" ++ leaseJson value.lease ++ ","
    ++ "\"used_generations\":" ++
      jsonArray (value.usedGenerations.map (fun generation => toString generation)) ++ ","
    ++ "\"now\":" ++ toString value.now ++ ","
    ++ "\"progress_seq\":" ++ toString value.progressSeq ++ ","
    ++ "\"continuation_required\":" ++ boolJson value.continuationRequired ++ ","
    ++ "\"token_charge_required\":" ++ boolJson value.tokenChargeRequired ++ ","
    ++ "\"continuation_count\":" ++ toString value.continuationCount ++ ","
    ++ "\"token_charge_count\":" ++ toString value.tokenChargeCount
    ++ "}"

def optionalWorldJson : Option (World Generation) → String
  | none => "null"
  | some value => worldJson value

def actionJson : Action Generation → String
  | .claim generation deadline =>
      "{\"kind\":\"claim\",\"generation\":" ++ toString generation ++
        ",\"deadline\":" ++ toString deadline ++ "}"
  | .begin generation =>
      "{\"kind\":\"begin\",\"generation\":" ++ toString generation ++ "}"
  | .persistProgress generation kind deadline =>
      "{\"kind\":\"persist_progress\",\"generation\":" ++ toString generation ++
        ",\"progress_kind\":" ++ jsonString (progressKindName kind) ++
        ",\"deadline\":" ++ toString deadline ++ "}"
  | .socketTraffic generation =>
      "{\"kind\":\"socket_traffic\",\"generation\":" ++ toString generation ++ "}"
  | .noOp generation =>
      "{\"kind\":\"no_op\",\"generation\":" ++ toString generation ++ "}"
  | .advanceTime now =>
      "{\"kind\":\"advance_time\",\"now\":" ++ toString now ++ "}"
  | .drop generation =>
      "{\"kind\":\"drop\",\"generation\":" ++ toString generation ++ "}"
  | .expire generation =>
      "{\"kind\":\"expire\",\"generation\":" ++ toString generation ++ "}"
  | .recover expected fresh deadline =>
      "{\"kind\":\"recover\",\"expected_generation\":" ++ toString expected ++
        ",\"fresh_generation\":" ++ toString fresh ++
        ",\"deadline\":" ++ toString deadline ++ "}"
  | .finalize generation outcome =>
      "{\"kind\":\"finalize\",\"generation\":" ++ toString generation ++
        ",\"outcome\":" ++ jsonString (outcomeName outcome) ++ "}"
  | .revoke expected deadline progress fresh outcome =>
      "{\"kind\":\"revoke\",\"expected_generation\":" ++ toString expected ++
        ",\"expected_deadline\":" ++ toString deadline ++
        ",\"expected_progress\":" ++ toString progress ++
        ",\"fresh_generation\":" ++ toString fresh ++
        ",\"outcome\":" ++ jsonString (outcomeName outcome) ++ "}"
  | .recoverAndFail expected fresh =>
      "{\"kind\":\"recover_and_fail\",\"expected_generation\":" ++
        toString expected ++ ",\"fresh_generation\":" ++ toString fresh ++ "}"

def leaseCaseJson (testCase : LeaseCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString testCase.name ++ ","
    ++ "\"pre\":" ++ worldJson testCase.pre ++ ","
    ++ "\"action\":" ++ actionJson testCase.action ++ ","
    ++ "\"expected\":" ++ optionalWorldJson testCase.expected
    ++ "}"

def leaseCasesJson : String :=
  jsonArray (leaseCases.map leaseCaseJson)

def leaseTraceCaseJson (testCase : LeaseTraceCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString testCase.name ++ ","
    ++ "\"pre\":" ++ worldJson testCase.pre ++ ","
    ++ "\"actions\":" ++ jsonArray (testCase.actions.map actionJson) ++ ","
    ++ "\"expected\":" ++ optionalWorldJson testCase.expected
    ++ "}"

def leaseTraceCasesJson : String :=
  jsonArray (leaseTraceCases.map leaseTraceCaseJson)

structure ProviderEofCase where
  sawExplicitFinal : Bool
  expectedFailure : Bool
  deriving DecidableEq, Repr

def providerEofCases : List ProviderEofCase :=
  [⟨false, true⟩, ⟨true, false⟩]

theorem providerEofCases_hold :
    providerEofCases.all (fun c =>
      providerEofIsFailure c.sawExplicitFinal == c.expectedFailure) = true := by
  native_decide

def providerEofCasesJson : String :=
  jsonArray (providerEofCases.map (fun c =>
    "{\"saw_explicit_final\":" ++ boolJson c.sawExplicitFinal ++
      ",\"expected_failure\":" ++ boolJson c.expectedFailure ++ "}"))

end Conformance.RequestExecutionLeaseContracts
