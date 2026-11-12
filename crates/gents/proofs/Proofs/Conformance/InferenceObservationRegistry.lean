import Proofs.Inference.ObservationRegistry
import Proofs.Conformance.Family

namespace Conformance.InferenceObservationRegistry

open ObservationRegistry

deriving instance Lean.ToJson for Activity

inductive StepKind where
  | openRequest
  | closeRequest
  | openTool
  | closeTool
  deriving Lean.ToJson

structure Step where
  kind : StepKind
  request : Nat
  tool : String
  deriving Lean.ToJson

structure Run where
  steps : List Step
  deriving Lean.ToJson

structure Outcome where
  activities : List (Option Activity)
  deriving Lean.ToJson

def Step.toAction (step : Step) : Action :=
  match step.kind with
  | .openRequest => .openRequest step.request
  | .closeRequest => .closeRequest step.request
  | .openTool => .openTool step.request step.tool
  | .closeTool => .closeTool step.request step.tool

def beginRequest (request : Nat) : Step := ⟨.openRequest, request, ""⟩
def endRequest (request : Nat) : Step := ⟨.closeRequest, request, ""⟩
def beginTool (request : Nat) (tool : String) : Step := ⟨.openTool, request, tool⟩
def endTool (request : Nat) (tool : String) : Step := ⟨.closeTool, request, tool⟩

def runs : List Run := [
  ⟨[beginRequest 0, beginTool 0 "first", beginTool 0 "second", endTool 0 "first",
    endTool 0 "second", endRequest 0]⟩,
  ⟨[beginRequest 0, beginTool 0 "call", endRequest 0, beginRequest 1, beginTool 1 "call",
    endTool 0 "call", endTool 1 "call", endRequest 1]⟩,
  ⟨[beginRequest 0, beginRequest 1, beginTool 1 "a", endRequest 1, beginTool 0 "a",
    endRequest 0]⟩,
  ⟨[beginRequest 0, endTool 0 "missing", endRequest 0]⟩,
  ⟨[beginRequest 0, beginTool 0 "a", endTool 0 "a", beginTool 0 "b", endRequest 0,
    endTool 0 "b"]⟩
]

@[conformance_family]
def activity : Conformance.Family Run Outcome where
  inputs := runs
  model := fun run => ⟨replay none (run.steps.map Step.toAction)⟩

end Conformance.InferenceObservationRegistry
