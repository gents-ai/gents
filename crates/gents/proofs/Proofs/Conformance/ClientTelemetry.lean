import Proofs.Clients.ClientShell.Telemetry
import Proofs.Conformance.Family

namespace Conformance.ClientTelemetry

open Lean _root_.ClientTelemetry

deriving instance ToJson for State

inductive ActionKind where
  | «open» | enable | disable | restart
  deriving ToJson

structure ActionWire where
  kind : ActionKind
  writeOk : Option Bool
  diskAfter : Option Bool
  deriving ToJson

structure TraceInput where
  initial : State
  actions : List ActionWire
  deriving ToJson

structure StepObservation where
  state : State
  send : Bool
  deriving ToJson

def ActionWire.ofModel : Action → ActionWire
  | .open => ⟨.open, none, none⟩
  | .enable writeOk diskAfter => ⟨.enable, some writeOk, some diskAfter⟩
  | .disable writeOk diskAfter => ⟨.disable, some writeOk, some diskAfter⟩
  | .restart => ⟨.restart, none, none⟩

def ActionWire.toModel (a : ActionWire) : Action :=
  match a.kind with
  | .open => .open
  | .enable => .enable (a.writeOk.getD true) (a.diskAfter.getD true)
  | .disable => .disable (a.writeOk.getD true) (a.diskAfter.getD false)
  | .restart => .restart

def observeTrace (state : State) : List Action → List StepObservation
  | [] => []
  | action :: rest =>
      let (next, send) := step state action
      { state := next, send } :: observeTrace next rest

def traceActions : List (List Action) :=
  [ [.open, .enable true true, .open, .restart, .open, .open, .disable true false, .restart, .open]
  , [.enable true true, .open, .disable true false, .restart, .open]
  , [.open, .disable true false, .restart, .enable true true, .open]
  , [.disable false false, .restart, .open]
  , [.enable false false, .open]
  , [.open, .enable true true] ]

@[conformance_family]
def trace : Family TraceInput (List StepObservation) where
  inputs := [false, true].flatMap fun configured =>
    [false, true].flatMap fun saved =>
      traceActions.map fun actions =>
        { initial := ⟨configured, saved, saved, false, false⟩
          actions := actions.map ActionWire.ofModel }
  model := fun i => observeTrace i.initial (i.actions.map ActionWire.toModel)

end Conformance.ClientTelemetry
