import Proofs.Shared.CompletionRetry.InvalidToolProgress
import Proofs.Shared.CompletionRetry.RepeatedToolFailure
import Proofs.Conformance.Family

namespace Conformance.InvalidToolProgressContracts
open Lean CompletionRetry.InvalidToolProgress

structure Case where
  name : String
  outcomes : List Outcome
  /-- The configured `max_invalid_tool_calls`; `none` is unset. -/
  maxInvalidToolCalls : Option Nat := none
  deriving Repr

def defaultLimitCases : List (String × List Outcome) :=
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

def cases : List Case :=
  defaultLimitCases.map (fun (name, outcomes) => { name := name, outcomes := outcomes }) ++
  [ { name := "configured_one_stops_on_first_invalid", maxInvalidToolCalls := some 1,
      outcomes := [.success, .unknownTool, .success] }
  , { name := "configured_one_leaves_valid_calls_running", maxInvalidToolCalls := some 1,
      outcomes := List.replicate 4 .success ++ List.replicate 3 .ordinaryFailure }
  , { name := "configured_three_exhausts_on_third", maxInvalidToolCalls := some 3,
      outcomes := List.replicate 4 .invalidArguments }
  , { name := "configured_twelve_runs_past_default", maxInvalidToolCalls := some 12,
      outcomes := List.replicate 11 .policyDenied ++ [.success] } ]

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
  maxInvalidToolCalls : Option Nat
  deriving ToJson

structure ComposedOutput where
  composedActions : List CompletionRetry.RepeatedToolFailure.Action
  composedEnding : Option CompletionRetry.RepeatedToolFailure.Ending
  composedInvalidUsed : Nat
  deriving ToJson

@[conformance_family]
def composed : Family ComposedInput ComposedOutput where
  inputs := cases.map fun c =>
    { name := c.name, outcomes := c.outcomes, maxInvalidToolCalls := c.maxInvalidToolCalls }
  model := fun i =>
    let start : CompletionRetry.RepeatedToolFailure.Composed :=
      { invalid := { limit := i.maxInvalidToolCalls.getD defaultLimit } }
    let (actions, ending) := CompletionRetry.RepeatedToolFailure.run start (composedEvents i.outcomes)
    { composedActions := actions
      composedEnding := ending
      composedInvalidUsed :=
        CompletionRetry.RepeatedToolFailure.invalidUsed start (composedEvents i.outcomes) }

/-- The `InferenceExecution` field a loop guard's limit is configured by. -/
inductive LimitField where
  | maxInvalidToolCalls
  | repeatedToolFailureLimit
  deriving DecidableEq, Repr, ToJson

def LimitField.default : LimitField → Nat
  | .maxInvalidToolCalls => defaultLimit
  | .repeatedToolFailureLimit => CompletionRetry.RepeatedToolFailure.defaultFailureLimit

structure LimitInput where
  field : LimitField
  configured : Option Int
  deriving ToJson

/-- Each field unset, refused (zero, negative) and at positive values on both
sides of its default; `none` is a refused value. -/
@[conformance_family]
def configured : Family LimitInput (Option Nat) where
  inputs := [LimitField.maxInvalidToolCalls, .repeatedToolFailureLimit].flatMap fun field =>
    [none, some 0, some (-1), some 1, some 2, some 3, some 8, some 1000].map fun configured =>
      { field := field, configured := configured }
  model := fun i => configuredLimit i.field.default i.configured

end Conformance.InvalidToolProgressContracts
