import Proofs.CompletionRetry.RepeatedToolFailure
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.RepeatedToolFailureContracts
open CompletionRetry.RepeatedToolFailure Conformance.Contracts

structure Case where
  name : String
  /-- Calls in provider order, each with the outcome it returns if dispatched. -/
  events : List Event
  deriving Repr

private def fail (call error : Nat) : Event := ⟨call, .ordinaryFailure, some error⟩
private def unidentified (call : Nat) : Event := ⟨call, .ordinaryFailure, none⟩
private def ok (call : Nat) : Event := ⟨call, .success, none⟩
private def skipped (call : Nat) : Event := ⟨call, .skipped, none⟩
private def invalidArgs (call : Nat) : Event := ⟨call, .invalidArguments, none⟩

def cases : List Case :=
  [ ⟨"single_failure", [fail 0 0]⟩
  , ⟨"limit_identical_failures_dispatch", List.replicate 3 (fail 0 0)⟩
  , ⟨"next_identical_repeat_suppressed", List.replicate 4 (fail 0 0)⟩
  , ⟨"second_identical_repeat_stops", List.replicate 5 (fail 0 0)⟩
  , ⟨"unbounded_identical_repeats_stop_on_fifth", List.replicate 12 (fail 0 0)⟩
  , ⟨"changed_error_restarts_streak",
      [fail 0 0, fail 0 0, fail 0 1, fail 0 1, fail 0 1, fail 0 1]⟩
  , ⟨"changed_arguments_dispatch", List.replicate 3 (fail 0 0) ++ [fail 1 0, fail 1 0]⟩
  , ⟨"success_resets_streak", [fail 0 0, fail 0 0, ok 0, fail 0 0, fail 0 0, fail 0 0]⟩
  , ⟨"different_call_after_suppression_resets",
      List.replicate 4 (fail 0 0) ++ [ok 1, fail 0 0, fail 0 0]⟩
  , ⟨"interleaved_repeats_are_not_consecutive",
      [fail 0 0, fail 1 0, fail 0 0, fail 1 0, fail 0 0, fail 1 0, fail 0 0]⟩
  , ⟨"hook_handled_call_restarts_streak",
      [fail 0 0, skipped 9, fail 0 0, skipped 9, fail 0 0, skipped 9, fail 0 0,
       skipped 9, fail 0 0]⟩
  , ⟨"invalid_class_repeats_are_left_to_the_allowance",
      List.replicate 5 (invalidArgs 0)⟩
  , ⟨"unidentified_failures_do_not_count", List.replicate 6 (unidentified 0)⟩
  , ⟨"suppression_charges_the_invalid_allowance",
      (List.range 7).map (fun call => invalidArgs (call + 1)) ++ List.replicate 5 (fail 0 0)⟩ ]

private def endingJson : Option Ending → String
  | none => "null"
  | some ending => jsonString (endingName ending)

private def outcomeString : CompletionRetry.InvalidToolProgress.Outcome → String
  | .invalidArguments => "invalidArguments"
  | .policyDenied => "policyDenied"
  | .unknownTool => "unknownTool"
  | .success => "success"
  | .ordinaryFailure => "ordinaryFailure"
  | .skipped => "skipped"
  | .backgroundCompletion => "backgroundCompletion"

private def eventJson (event : Event) : String :=
  "{\"call\":" ++ toString event.call ++
  ",\"outcome\":" ++ jsonString (outcomeString event.outcome) ++
  ",\"error\":" ++ jsonOptionalNat event.error ++ "}"

/-- Expected actions and ending are computed by the model for each trace. -/
private def caseJson (c : Case) : String :=
  let (actions, ending) := run {} c.events
  "{\"name\":" ++ jsonString c.name ++
  ",\"events\":" ++ jsonArray (c.events.map eventJson) ++
  ",\"expected_actions\":" ++ jsonArray (actions.map (jsonString ∘ actionName)) ++
  ",\"expected_ending\":" ++ endingJson ending ++
  ",\"expected_invalid_used\":" ++ toString (invalidUsed {} c.events) ++ "}"

def casesJson := jsonArray (cases.map caseJson)

theorem cases_cover_every_action_and_ending :
    let runs := cases.map fun c => run {} c.events
    [Action.dispatch, .suppress, .skip, .stop].all
        (fun a => runs.any fun r => r.1.contains a) ∧
      [some Ending.repeatedFailure, some .invalidExhausted, none].all
        (fun e => runs.any fun r => r.2 == e) := by
  decide

end Conformance.RepeatedToolFailureContracts
