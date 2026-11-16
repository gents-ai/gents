import Proofs.Shared.CompletionRetry.RepeatedToolFailure
import Proofs.Conformance.Family

namespace Conformance.RepeatedToolFailureContracts
open Lean CompletionRetry.RepeatedToolFailure

structure Case where
  name : String
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

deriving instance ToJson for CompletionRetry.InvalidToolProgress.Outcome, Event, Action, Ending

structure Input where
  events : List Event
  deriving ToJson

structure Output where
  actions : List Action
  ending : Option Ending
  invalidUsed : Nat
  deriving ToJson

@[conformance_family]
def ownedLoop : Family Input Output where
  inputs := cases.map fun c => { events := c.events }
  model := fun i =>
    let (actions, ending) := run {} i.events
    { actions, ending, invalidUsed := invalidUsed {} i.events }

theorem cases_cover_every_action_and_ending :
    let runs := cases.map fun c => run {} c.events
    [Action.dispatch, .suppress, .skip, .stop].all
        (fun a => runs.any fun r => r.1.contains a) ∧
      [some Ending.repeatedFailure, some .invalidExhausted, none].all
        (fun e => runs.any fun r => r.2 == e) := by
  decide

end Conformance.RepeatedToolFailureContracts
