import Proofs.Clients.ClientShell.Installation
import Proofs.Conformance.Family
import Lean

namespace Conformance.ClientInstallation
open Lean _root_.ClientInstallation

deriving instance ToJson for Phase

structure CompatibilityInput where
  protocolMatches : Bool
  frameLimitMatches : Bool
  requiredCommands : List String
  availableCommands : List String
  deriving ToJson

private def commandSets : List (List String × List String) :=
  [([], []), (["a"], ["a"]), (["a"], ["a", "b"]), (["a", "b"], ["a"]), (["a"], [])]

@[conformance_family]
def compatibility : Conformance.Family CompatibilityInput Bool where
  inputs := [false, true].flatMap fun protocolMatches =>
    [false, true].flatMap fun frameLimitMatches =>
      commandSets.map fun (requiredCommands, availableCommands) =>
        { protocolMatches, frameLimitMatches, requiredCommands, availableCommands }
  model := fun i =>
    canReuse i.protocolMatches i.frameLimitMatches i.requiredCommands i.availableCommands

inductive ActionKind where
  | attach
  | detach
  | retire
  | release
  | close
  | acknowledge
  | replace
  deriving ToJson

/-- `startupReserved` is read only by `replace`; every other action carries `false`. -/
structure ActionWire where
  kind : ActionKind
  startupReserved : Bool
  deriving ToJson

def ActionWire.toAction (action : ActionWire) : Action :=
  match action.kind with
  | .attach => .attach
  | .detach => .detach
  | .retire => .retire
  | .release => .release
  | .close => .close
  | .acknowledge => .acknowledge
  | .replace => .replace action.startupReserved

structure StateWire where
  phase : Phase
  attachments : Nat
  deriving ToJson

structure TraceStep where
  accepted : Bool
  state : StateWire
  deriving ToJson

structure TraceInput where
  phase : Phase
  attachments : Nat
  trace : List ActionWire
  deriving ToJson

private def runSteps : State → List Action → List TraceStep
  | _, [] => []
  | state, action :: rest =>
    let result := step state action
    let next := result.getD state
    ({ accepted := result.isSome,
       state := { phase := next.phase, attachments := next.attachments } } : TraceStep)
      :: runSteps next rest

private def initialState (phase : Phase) (attachments : Nat) : State :=
  { phase, history := 11, attachments }

private def act (kind : ActionKind) : ActionWire := { kind, startupReserved := false }

private def replaceAct (startupReserved : Bool) : ActionWire :=
  { kind := .replace, startupReserved }

private def actionWires : List ActionWire :=
  [act .attach, act .detach, act .retire, act .release, act .close, act .acknowledge,
    replaceAct false, replaceAct true]

private def actionTraces : List (List ActionWire) :=
  actionWires.flatMap fun first => actionWires.flatMap fun second =>
    actionWires.map fun third => [first, second, third]

private def retirementInput : TraceInput :=
  { phase := .accepting, attachments := 2,
    trace := [act .retire, act .attach, act .acknowledge, replaceAct true, act .detach,
      act .release, act .acknowledge, act .detach, act .release, act .close, act .acknowledge,
      replaceAct false, replaceAct true] }

private def generatedInputs : List TraceInput :=
  [Phase.accepting, .draining, .released, .closed].flatMap fun phase =>
    (List.range 3).flatMap fun attachments =>
      actionTraces.map fun trace => { phase, attachments, trace }

@[conformance_family]
def traces : Conformance.Family TraceInput (List TraceStep) where
  inputs := retirementInput :: generatedInputs
  model := fun i =>
    runSteps (initialState i.phase i.attachments) (i.trace.map ActionWire.toAction)

end Conformance.ClientInstallation
