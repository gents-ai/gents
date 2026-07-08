import Proofs.CompletionRetry.Executable
import Proofs.Conformance.ContractTypes

/-!
# CompletionRetry Conformance Contracts

Finite executable witnesses for the per-completion retry machine. Each row is
computed by running `CompletionRetry.step?` against a concrete state/action
pair; Rust consumes the resulting rows through `tests/conformance`.
-/

namespace CompletionRetry.Contracts

open Conformance.Contracts

def failureClassVocabulary : List String :=
  ["transport", "parse_bad_request", "permanent"]

def failureClassName : FailureClass → String
  | .transport => "transport"
  | .parseBadRequest => "parse_bad_request"
  | .permanent => "permanent"

def phaseName : Phase → String
  | .issuing => "issuing"
  | .streaming => "streaming"
  | .backingOff _ => "backing_off"
  | .repairing => "repairing"
  | .turnClosed => "turn_closed"
  | .turnDone => "turn_done"
  | .exhausted => "exhausted"
  | .failedPermanent => "failed_permanent"

def boolJson (value : Bool) : String :=
  if value then "true" else "false"

def jsonOptionalNat : Option Nat → String
  | none => "null"
  | some value => toString value

def jsonOptionalBool : Option Bool → String
  | none => "null"
  | some value => boolJson value

def jsonOptionalPhase : Option Phase → String
  | none => "null"
  | some phase => jsonString (phaseName phase)

def jsonOptionalFailureClass : Option FailureClass → String
  | none => "null"
  | some klass => jsonString (failureClassName klass)

def defaultBudget : Budget :=
  { transportRetries := 3, resampleRetries := 1, allowRepair := true }

def baseState
    (phase : Phase := .streaming)
    (budget : Budget := defaultBudget)
    (transportUsed : Nat := 0)
    (resampleUsed : Nat := 0)
    (repairUsed : Bool := false)
    (lastParseError : Option String := none)
    (now : Time := 10)
    (deadline : Option Time := none)
    (turnIndex : Nat := 0)
    (effects : Nat := 0)
    (rendered : Nat := 0) : State :=
  { phase := phase
  , budget := budget
  , transportUsed := transportUsed
  , resampleUsed := resampleUsed
  , repairUsed := repairUsed
  , lastParseError := lastParseError
  , now := now
  , deadline := deadline
  , turn := { turnIndex := turnIndex, effects := effects, rendered := rendered }
  }

structure CompletionRetryCase where
  name : String
  action : String
  rustSurface : String
  failureClass : Option FailureClass
  selectedWake : Option Time
  pre : State
  post : Option State
  intermediate : Option State := none
  deriving Repr

def caseFromStep
    (name action rustSurface : String)
    (failureClass : Option FailureClass)
    (selectedWake : Option Time)
    (pre : State)
    (actionValue : Action) : CompletionRetryCase :=
  { name := name
  , action := action
  , rustSurface := rustSurface
  , failureClass := failureClass
  , selectedWake := selectedWake
  , pre := pre
  , post := step? pre actionValue
  }

def caseCloseTurnThenContinue : CompletionRetryCase :=
  let pre := baseState (effects := 1)
  let intermediate := step? pre .closeTurn
  let post := intermediate.bind (fun closed => step? closed (.continueAfterClose 12))
  { name := "close_turn_with_effects_legal"
  , action := "close_turn_then_continue"
  , rustSurface := "mid_stream_effects_close_and_continue"
  , failureClass := none
  , selectedWake := some 12
  , pre := pre
  , intermediate := intermediate
  , post := post
  }

def cases : List CompletionRetryCase :=
  [ caseFromStep
      "transport_ladder_progresses"
      "pre_stream_fail"
      "pre_stream_transport_retry"
      (some .transport)
      (some 12)
      (baseState (budget := { transportRetries := 3, resampleRetries := 1, allowRepair := true }))
      (.preStreamFail .transport "transport" 12)
  , caseFromStep
      "transport_exhausts_after_budget"
      "pre_stream_fail"
      "pre_stream_transport_fail"
      (some .transport)
      (some 12)
      (baseState
        (budget := { transportRetries := 3, resampleRetries := 1, allowRepair := true })
        (transportUsed := 3))
      (.preStreamFail .transport "transport" 12)
  , caseFromStep
      "selected_delay_past_deadline_fails_fast"
      "pre_stream_fail"
      "pre_stream_transport_fail"
      (some .transport)
      (some 20)
      (baseState
        (deadline := some 15)
        (budget := { transportRetries := 3, resampleRetries := 1, allowRepair := true }))
      (.preStreamFail .transport "transport" 20)
  , caseFromStep
      "deadline_behind_clock_fails_fast"
      "pre_stream_fail"
      "pre_stream_transport_fail"
      (some .transport)
      (some 10)
      (baseState
        (now := 10)
        (deadline := some 5)
        (budget := { transportRetries := 3, resampleRetries := 1, allowRepair := true }))
      (.preStreamFail .transport "transport" 10)
  , caseFromStep
      "deterministic_400_skips_to_repair"
      "pre_stream_fail"
      "pre_stream_parse_repair"
      (some .parseBadRequest)
      (some 12)
      (baseState
        (budget := { transportRetries := 3, resampleRetries := 2, allowRepair := true })
        (resampleUsed := 1)
        (lastParseError := some "json-parse"))
      (.preStreamFail .parseBadRequest "json-parse" 12)
  , caseFromStep
      "repair_second_time_illegal"
      "repair_issue"
      "repair_already_used_fails"
      none
      none
      (baseState (phase := .repairing) (repairUsed := true))
      .repairIssue
  , caseFromStep
      "retract_with_effects_illegal"
      "retract"
      "mid_stream_effects_not_retract"
      none
      (some 12)
      (baseState (effects := 1))
      (.retract 12)
  , caseCloseTurnThenContinue
  , caseFromStep
      "reissue_with_open_effects_illegal"
      "pre_stream_fail"
      "model_only_open_effects_guard"
      (some .transport)
      (some 12)
      (baseState (effects := 1))
      (.preStreamFail .transport "transport" 12)
  , caseFromStep
      "rendered_never_two"
      "stream_ok"
      "model_only_stream_ok"
      none
      none
      (baseState (rendered := 0))
      .streamOk
  , caseFromStep
      "permanent_class_cannot_backoff"
      "pre_stream_fail"
      "pre_stream_permanent_fail"
      (some .permanent)
      (some 12)
      (baseState)
      (.preStreamFail .permanent "permanent" 12)
  ]

def CompletionRetryCase.toJson (c : CompletionRetryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"domain\":\"completionRetry\","
    ++ "\"action\":" ++ jsonString c.action ++ ","
    ++ "\"rust_surface\":" ++ jsonString c.rustSurface ++ ","
    ++ "\"failure_class\":" ++ jsonOptionalFailureClass c.failureClass ++ ","
    ++ "\"selected_wake\":" ++ jsonOptionalNat c.selectedWake ++ ","
    ++ "\"legal\":" ++ boolJson c.post.isSome ++ ","
    ++ "\"pre_phase\":" ++ jsonString (phaseName c.pre.phase) ++ ","
    ++ "\"expected_phase\":" ++ jsonOptionalPhase (c.post.map (fun s => s.phase)) ++ ","
    ++ "\"intermediate_phase\":"
      ++ jsonOptionalPhase (c.intermediate.map (fun s => s.phase)) ++ ","
    ++ "\"expected_transport_used\":"
      ++ jsonOptionalNat (c.post.map (fun s => s.transportUsed)) ++ ","
    ++ "\"expected_resample_used\":"
      ++ jsonOptionalNat (c.post.map (fun s => s.resampleUsed)) ++ ","
    ++ "\"expected_repair_used\":"
      ++ jsonOptionalBool (c.post.map (fun s => s.repairUsed)) ++ ","
    ++ "\"expected_last_parse_error\":"
      ++ jsonOptionalString (c.post.bind (fun s => s.lastParseError)) ++ ","
    ++ "\"expected_turn_index\":"
      ++ jsonOptionalNat (c.post.map (fun s => s.turn.turnIndex)) ++ ","
    ++ "\"intermediate_turn_index\":"
      ++ jsonOptionalNat (c.intermediate.map (fun s => s.turn.turnIndex)) ++ ","
    ++ "\"expected_effects\":" ++ jsonOptionalNat (c.post.map (fun s => s.turn.effects)) ++ ","
    ++ "\"expected_rendered\":"
      ++ jsonOptionalNat (c.post.map (fun s => s.turn.rendered)) ++ ","
    ++ "\"intermediate_rendered\":"
      ++ jsonOptionalNat (c.intermediate.map (fun s => s.turn.rendered))
    ++ "}"

def casesJson : String :=
  jsonArray (cases.map CompletionRetryCase.toJson)

end CompletionRetry.Contracts
