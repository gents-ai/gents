import Proofs.Runtime.RuntimeReconcile.ExecutionRegistry
import Proofs.Conformance.Family

namespace Background.ExecutionRegistry
deriving instance Lean.ToJson for Refusal
end Background.ExecutionRegistry

namespace Conformance.BackgroundExecutionRegistry

open Lean Background.ExecutionRegistry

inductive StepKind where
  | reserve | release | retire
  deriving DecidableEq, Repr, ToJson

structure RegistryStep where
  kind : StepKind
  doc : String
  operatorAuthenticated : Bool
  observedBuildMatches : Bool
  deriving ToJson

structure Trace where
  steps : List RegistryStep
  deriving ToJson

structure StepResult where
  refusal : Option Refusal
  live : List String
  deriving ToJson

structure Outcome where
  results : List StepResult
  deriving ToJson

private def toAction (s : RegistryStep) : Action :=
  match s.kind with
  | .reserve => .reserve s.doc
  | .release => .release s.doc
  | .retire => .retire ⟨s.operatorAuthenticated, s.observedBuildMatches⟩

private def sorted (docs : List String) : List String :=
  docs.mergeSort fun a b => decide (a ≤ b)

private def reserveStep (doc : String) : RegistryStep := ⟨.reserve, doc, false, false⟩
private def releaseStep (doc : String) : RegistryStep := ⟨.release, doc, false, false⟩
private def retireStep (authenticated buildMatches : Bool) : RegistryStep :=
  ⟨.retire, "", authenticated, buildMatches⟩

private def boundaries : List Trace :=
  [ ⟨[reserveStep "owned-doc", retireStep true true, releaseStep "owned-doc", retireStep true true,
      reserveStep "refused-doc"]⟩
  , ⟨[reserveStep "old-doc", reserveStep "new-doc", releaseStep "new-doc", retireStep true true,
      releaseStep "old-doc", retireStep true true]⟩
  , ⟨[reserveStep "doc", reserveStep "doc", releaseStep "doc", retireStep true true]⟩ ]

private def alphabet : List RegistryStep :=
  [reserveStep "a", reserveStep "b", releaseStep "a", releaseStep "b",
   retireStep true true, retireStep false true, retireStep true false]

private def generated : List Trace :=
  alphabet.flatMap fun first => alphabet.flatMap fun second => alphabet.map fun third =>
    ⟨[first, second, third]⟩

@[conformance_family]
def traces : Conformance.Family Trace Outcome where
  inputs := boundaries ++ generated
  model := fun t =>
    { results := (run empty (t.steps.map toAction)).map fun (refusal, live) =>
        { refusal, live := sorted live } }

end Conformance.BackgroundExecutionRegistry
