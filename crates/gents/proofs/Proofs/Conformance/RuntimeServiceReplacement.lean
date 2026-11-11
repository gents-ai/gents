import Proofs.Runtime.RuntimeReconcile.ServiceReplacement
import Proofs.Conformance.Family
import Lean

namespace RuntimeReconcile.ServiceReplacement
deriving instance Lean.ToJson for Phase
end RuntimeReconcile.ServiceReplacement

namespace RuntimeReconcile.ServiceReplacement
deriving instance Lean.ToJson for State
end RuntimeReconcile.ServiceReplacement

namespace Conformance.RuntimeServiceReplacement

open Lean _root_.RuntimeReconcile.ServiceReplacement

inductive ActionKind where
  | admit | finish | nativeStarted | nativeReturned | retire | release | replace

deriving instance Lean.ToJson for ActionKind

structure ReplacementStep where
  kind : ActionKind
  operatorAuthenticated : Bool
  observedBuildMatches : Bool
  storeReleased : Bool
  startupReserved : Bool
  deriving ToJson

structure Scenario where
  initial : State
  steps : List ReplacementStep
  deriving ToJson

structure StepResult where
  accepted : Bool
  state : State
  deriving ToJson

structure Outcome where
  results : List StepResult
  deriving ToJson

private def toAction (s : ReplacementStep) : Action :=
  match s.kind with
  | .admit => .admit
  | .finish => .finish
  | .nativeStarted => .nativeStarted
  | .nativeReturned => .nativeReturned
  | .retire => .retire ⟨s.operatorAuthenticated, s.observedBuildMatches⟩
  | .release => .release s.storeReleased
  | .replace => .replace s.startupReserved

private def traceResults : State → List Action → List StepResult
  | _, [] => []
  | state, action :: rest =>
    match step state action with
    | some next => { accepted := true, state := next } :: traceResults next rest
    | none => { accepted := false, state := state } :: traceResults state rest

private def plainStep (kind : ActionKind) : ReplacementStep :=
  ⟨kind, false, false, false, false⟩

private def retireStep (authenticated buildMatches : Bool) : ReplacementStep :=
  ⟨.retire, authenticated, buildMatches, false, false⟩

private def releaseStep (stored : Bool) : ReplacementStep :=
  ⟨.release, false, false, stored, false⟩

private def replaceStep (reserved : Bool) : ReplacementStep :=
  ⟨.replace, false, false, false, reserved⟩

private def retirements : List ReplacementStep :=
  [false, true].flatMap fun authenticated =>
    [false, true].map fun buildMatches => retireStep authenticated buildMatches

private def actions : List ReplacementStep :=
  [plainStep .admit, plainStep .finish, plainStep .nativeStarted, plainStep .nativeReturned,
   releaseStep false, releaseStep true, replaceStep false, replaceStep true] ++ retirements

private def boundaries : List Scenario :=
  [ { initial := ⟨.accepting, 0, 0⟩,
      steps := [plainStep .admit, retireStep true true, plainStep .finish, retireStep true true,
        plainStep .admit, releaseStep false, plainStep .nativeStarted, releaseStep true,
        plainStep .nativeReturned, releaseStep true, replaceStep false, replaceStep true] }
  , { initial := ⟨.accepting, 0, 0⟩,
      steps := [retireStep true true, retireStep true true, plainStep .admit, releaseStep true,
        replaceStep true, replaceStep true] } ]

private def generated : List Scenario :=
  [Phase.accepting, .stopping, .released].flatMap fun phase =>
    (List.range 3).flatMap fun executions =>
      (List.range 3).flatMap fun nativeOutstanding =>
        actions.flatMap fun first => actions.map fun second =>
          { initial := { phase := phase, executions := executions, nativeOutstanding := nativeOutstanding },
            steps := [first, second] }

@[conformance_family]
def traces : Conformance.Family Scenario Outcome where
  inputs := boundaries ++ generated
  model := fun s => { results := traceResults s.initial (s.steps.map toAction) }

end Conformance.RuntimeServiceReplacement
