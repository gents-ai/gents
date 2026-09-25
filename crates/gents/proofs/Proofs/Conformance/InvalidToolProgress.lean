import Proofs.CompletionRetry.InvalidToolProgress
import Proofs.CompletionRetry.RepeatedToolFailure
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.InvalidToolProgressContracts
open CompletionRetry.InvalidToolProgress Conformance.Contracts

structure Case where
  name : String
  outcomes : List Outcome
  deriving Repr

def cases : List Case :=
  [ ⟨"empty", []⟩
  , ⟨"invalid_arguments_charged", [.invalidArguments]⟩
  , ⟨"policy_denial_charged", [.policyDenied]⟩
  , ⟨"unknown_tool_charged", [.unknownTool]⟩
  , ⟨"ordinary_failure_uncharged", List.replicate 12 .ordinaryFailure⟩
  , ⟨"seven_invalids_allow_next", List.replicate 7 .policyDenied⟩
  , ⟨"eighth_invalid_exhausts", List.replicate 8 .invalidArguments⟩
  , ⟨"ninth_invalid_not_dispatched", List.replicate 9 .unknownTool⟩
  , ⟨"success_does_not_reset", [.policyDenied,.success,.invalidArguments,.success,
      .unknownTool,.success,.policyDenied,.success,.invalidArguments,.success,
      .unknownTool,.success,.policyDenied,.success,.invalidArguments]⟩
  , ⟨"ordinary_failure_does_not_reset", [.policyDenied,.ordinaryFailure,.invalidArguments,
      .ordinaryFailure,.unknownTool,.ordinaryFailure,.policyDenied,.ordinaryFailure,
      .invalidArguments,.ordinaryFailure,.unknownTool,.ordinaryFailure,.policyDenied,
      .ordinaryFailure,.invalidArguments]⟩
  , ⟨"success_after_exhaustion_not_dispatched",
      List.replicate 8 .policyDenied ++ [.success]⟩ ]

private def outcomeString : Outcome → String
  | .invalidArguments => "invalidArguments"
  | .policyDenied => "policyDenied"
  | .unknownTool => "unknownTool"
  | .success => "success"
  | .ordinaryFailure => "ordinaryFailure"
  | .skipped => "skipped"
  | .backgroundCompletion => "backgroundCompletion"

private def outcomeCall : Outcome → Nat
  | .invalidArguments => 0
  | .policyDenied => 1
  | .unknownTool => 2
  | .success => 3
  | .ordinaryFailure => 4
  | .skipped => 5
  | .backgroundCompletion => 6

/-- The owned loop composes this allowance with the repetition guard. A
fixture call's arguments name only its outcome, so equal outcomes are identical
calls, and every fixture ordinary failure has the same error. -/
def composedEvents (outcomes : List Outcome) : List CompletionRetry.RepeatedToolFailure.Event :=
  outcomes.map fun o => ⟨outcomeCall o, o, if o = .ordinaryFailure then some 0 else none⟩

private def composedJson (outcomes : List Outcome) : String :=
  let (actions, ending) := CompletionRetry.RepeatedToolFailure.run {} (composedEvents outcomes)
  ",\"composed_actions\":" ++
    jsonArray (actions.map (jsonString ∘ CompletionRetry.RepeatedToolFailure.actionName)) ++
  ",\"composed_ending\":" ++
    (match ending with
      | none => "null"
      | some ending => jsonString (CompletionRetry.RepeatedToolFailure.endingName ending)) ++
  ",\"composed_invalid_used\":" ++
    toString (CompletionRetry.RepeatedToolFailure.invalidUsed {} (composedEvents outcomes))

private def caseJson (c : Case) : String :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"outcomes\":" ++ jsonArray (c.outcomes.map (jsonString ∘ outcomeString)) ++
  composedJson c.outcomes ++ "}"

def casesJson := jsonArray (cases.map caseJson)
end Conformance.InvalidToolProgressContracts
