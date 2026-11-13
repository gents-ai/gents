import Proofs.Shared.CompletionRetry.InvalidToolProgress
import Proofs.Shared.CompletionRetry.RepeatedToolFailure
import Proofs.Conformance.Family

namespace Conformance.InvalidToolProgressContracts
open Lean CompletionRetry.InvalidToolProgress

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

private def outcomeCall : Outcome → Nat
  | .invalidArguments => 0
  | .policyDenied => 1
  | .unknownTool => 2
  | .success => 3
  | .ordinaryFailure => 4
  | .skipped => 5
  | .backgroundCompletion => 6

def composedEvents (outcomes : List Outcome) : List CompletionRetry.RepeatedToolFailure.Event :=
  outcomes.map fun o => ⟨outcomeCall o, o, if o = .ordinaryFailure then some 0 else none⟩

deriving instance ToJson for Outcome, CompletionRetry.RepeatedToolFailure.Action,
  CompletionRetry.RepeatedToolFailure.Ending

structure ComposedInput where
  name : String
  outcomes : List Outcome
  deriving ToJson

structure ComposedOutput where
  composedActions : List CompletionRetry.RepeatedToolFailure.Action
  composedEnding : Option CompletionRetry.RepeatedToolFailure.Ending
  composedInvalidUsed : Nat
  deriving ToJson

@[conformance_family]
def composed : Family ComposedInput ComposedOutput where
  inputs := cases.map fun c => { name := c.name, outcomes := c.outcomes }
  model := fun i =>
    let (actions, ending) := CompletionRetry.RepeatedToolFailure.run {} (composedEvents i.outcomes)
    { composedActions := actions
      composedEnding := ending
      composedInvalidUsed :=
        CompletionRetry.RepeatedToolFailure.invalidUsed {} (composedEvents i.outcomes) }

end Conformance.InvalidToolProgressContracts
