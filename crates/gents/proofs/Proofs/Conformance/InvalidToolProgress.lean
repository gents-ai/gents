import Proofs.CompletionRetry.InvalidToolProgress
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.InvalidToolProgressContracts
open CompletionRetry.InvalidToolProgress Conformance.Contracts

structure Case where
  name : String
  outcomes : List Outcome
  expectedInvalidUsed : Nat
  expectedExhausted : Bool
  expectedObservedOutcomes : Nat
  deriving Repr

/-- Prefix observed by the loop; no suffix can be dispatched after exhaustion. -/
def observed (s : State) : List Outcome → Nat
  | [] => 0
  | o :: rest => if canDispatch s then 1 + observed (recordDurable s o) rest else 0

def cases : List Case :=
  [ ⟨"empty", [], 0, false, 0⟩
  , ⟨"invalid_arguments_charged", [.invalidArguments], 1, false, 1⟩
  , ⟨"policy_denial_charged", [.policyDenied], 1, false, 1⟩
  , ⟨"unknown_tool_charged", [.unknownTool], 1, false, 1⟩
  , ⟨"ordinary_failure_uncharged", List.replicate 12 .ordinaryFailure, 0, false, 12⟩
  , ⟨"seven_invalids_allow_next", List.replicate 7 .policyDenied, 7, false, 7⟩
  , ⟨"eighth_invalid_exhausts", List.replicate 8 .invalidArguments, 8, true, 8⟩
  , ⟨"ninth_invalid_not_dispatched", List.replicate 9 .unknownTool, 8, true, 8⟩
  , ⟨"success_does_not_reset", [.policyDenied,.success,.invalidArguments,.success,
      .unknownTool,.success,.policyDenied,.success,.invalidArguments,.success,
      .unknownTool,.success,.policyDenied,.success,.invalidArguments], 8, true, 15⟩
  , ⟨"ordinary_failure_does_not_reset", [.policyDenied,.ordinaryFailure,.invalidArguments,
      .ordinaryFailure,.unknownTool,.ordinaryFailure,.policyDenied,.ordinaryFailure,
      .invalidArguments,.ordinaryFailure,.unknownTool,.ordinaryFailure,.policyDenied,
      .ordinaryFailure,.invalidArguments], 8, true, 15⟩
  , ⟨"success_after_exhaustion_not_dispatched",
      List.replicate 8 .policyDenied ++ [.success], 8, true, 8⟩ ]

theorem cases_match : ∀ c ∈ cases,
    (run ⟨0⟩ c.outcomes).invalidUsed = c.expectedInvalidUsed ∧
    exhausted (run ⟨0⟩ c.outcomes) = c.expectedExhausted ∧
    observed ⟨0⟩ c.outcomes = c.expectedObservedOutcomes := by decide

private def outcomeString : Outcome → String
  | .invalidArguments => "invalidArguments"
  | .policyDenied => "policyDenied"
  | .unknownTool => "unknownTool"
  | .success => "success"
  | .ordinaryFailure => "ordinaryFailure"
  | .skipped => "skipped"
  | .backgroundCompletion => "backgroundCompletion"

private def caseJson (c : Case) : String :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"outcomes\":" ++ jsonArray (c.outcomes.map (jsonString ∘ outcomeString)) ++
  ",\"expected_invalid_used\":" ++ toString c.expectedInvalidUsed ++
  ",\"expected_exhausted\":" ++ (if c.expectedExhausted then "true" else "false") ++
  ",\"expected_observed_outcomes\":" ++ toString c.expectedObservedOutcomes ++ "}"

def casesJson := jsonArray (cases.map caseJson)
end Conformance.InvalidToolProgressContracts
