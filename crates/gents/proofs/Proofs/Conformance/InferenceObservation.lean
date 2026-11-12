import Proofs.Clients.ClientShell.InferenceObservation
import Proofs.Conformance.Family

namespace Conformance.InferenceObservation
open ClientInferenceObservation

private def target : Target := ⟨"node", "session", "request", "generation"⟩
private def other : Target := ⟨"node", "other", "request", "generation"⟩
private def otherGeneration : Target := ⟨"node", "session", "request", "new-generation"⟩
private def initial : State := ⟨0, some target, false, none⟩

deriving instance Lean.ToJson for Target, Activity, Observation

inductive ActionKind where
  | receive
  | select
  | project
  deriving Lean.ToJson

structure ActionWire where
  kind : ActionKind
  epoch : Option Nat
  observation : Option Observation
  target : Option Target
  terminal : Option Bool
  now : Option Nat
  maxAge : Option Nat
  deriving Lean.ToJson

private def receiveAction (epoch : Nat) (observation : Observation) : ActionWire :=
  ⟨.receive, some epoch, some observation, none, none, none, none⟩

private def selectAction (target : Option Target) (terminal : Bool) : ActionWire :=
  ⟨.select, none, none, target, some terminal, none, none⟩

private def projectAction (now maxAge : Nat) : ActionWire :=
  ⟨.project, none, none, none, none, some now, some maxAge⟩

def stepWire (state : State) (action : ActionWire) : State × Option Activity :=
  match action.kind, action.epoch, action.observation, action.now, action.maxAge with
  | .receive, some epoch, some observation, _, _ =>
      let next := receive state epoch observation
      (next, next.observation.map (·.activity))
  | .select, _, _, _, _ =>
      let next := select state action.target (action.terminal.getD false)
      (next, next.observation.map (·.activity))
  | .project, _, _, some now, some maxAge => (state, project state now maxAge)
  | _, _, _, _, _ => (state, state.observation.map (·.activity))

def runTrace : State → List ActionWire → List (Option Activity)
  | _, [] => []
  | state, action :: rest =>
      let (next, observed) := stepWire state action
      observed :: runTrace next rest

structure TraceInput where
  actions : List ActionWire
  deriving Lean.ToJson

structure TraceOutput where
  expected : List (Option Activity)
  deriving Lean.ToJson

private def obs (attempt sequence time : Nat) (activity : Activity) : Observation :=
  ⟨target, 0, attempt, sequence, time, activity⟩

def visibilityCasesJson : String :=
  let cases : List (Nat × Nat × Bool) := [
    (0, 0, false), (0, 1, true), (1, 0, true), (1, 1, false),
    (1, 1, true), (2, 1, true), (1, 2, true)]
  let render (c : Nat × Nat × Bool) : String :=
    let (sessions, requests, active) := c
    "{\"sessions\":" ++ toString sessions ++ ",\"requests\":" ++ toString requests ++
      ",\"active\":" ++ (if active then "true" else "false") ++
      ",\"expected\":" ++ (if sourceVisible sessions requests active then "true" else "false") ++ "}"
  "[" ++ String.intercalate "," (cases.map render) ++ "]"

def routeCasesJson : String :=
  let cases : List (Bool × Nat × Nat × Bool) := [
    (false, 1, 1, true), (false, 0, 0, false), (true, 1, 1, true), (true, 0, 1, true),
    (true, 1, 1, false)]
  let name : ActivityResponse → String
    | .forbidden => "forbidden"
    | .unavailable => "unavailable"
    | .visible => "visible"
  let render (c : Bool × Nat × Nat × Bool) : String :=
    let (holds, sessions, requests, active) := c
    "{\"holds_node_read\":" ++ (if holds then "true" else "false") ++
      ",\"sessions\":" ++ toString sessions ++ ",\"requests\":" ++ toString requests ++
      ",\"active\":" ++ (if active then "true" else "false") ++
      ",\"expected\":\"" ++ name (activityResponse holds sessions requests active) ++ "\"}"
  "[" ++ String.intercalate "," (cases.map render) ++ "]"

def traces : List (List ActionWire) := [
  [receiveAction 0 (obs 0 1 10 .awaitingProvider), receiveAction 0 (obs 0 2 11 .retrying),
    receiveAction 0 (obs 1 3 12 .working), projectAction 15 5, projectAction 18 5],
  [receiveAction 0 (obs 1 4 10 .waiting), receiveAction 0 (obs 0 5 11 .working),
    receiveAction 0 (obs 1 3 12 .retrying), projectAction 9 5],
  [receiveAction 0 (obs 0 1 10 .working), selectAction (some other) false,
    receiveAction 0 (obs 0 2 11 .retrying), selectAction (some target) false,
    receiveAction 2 (obs 0 3 12 .waiting)],
  [receiveAction 0 (obs 0 1 10 .working), selectAction (some target) true,
    receiveAction 1 (obs 1 2 11 .retrying)],
  [receiveAction 0 (obs 0 1 10 .working), selectAction (some target) false,
    receiveAction 0 (obs 0 2 11 .working), projectAction 12 5],
  [receiveAction 0 (obs 2 1 10 .retrying),
    receiveAction 0 { (obs 0 2 11 .working) with turn := 1 },
    receiveAction 0 (obs 3 3 12 .retrying)],
  [receiveAction 0 (obs 0 1 10 .working),
    receiveAction 0 { (obs 0 2 11 .retrying) with target := otherGeneration },
    selectAction (some otherGeneration) false, receiveAction 1 (obs 1 3 12 .waiting)]
]

/-- A client follows one selected request: each step's observation is what the
client shows after it. -/
@[conformance_family]
def trace : Conformance.Family TraceInput TraceOutput where
  inputs := traces.map fun actions => { actions }
  model := fun i => { expected := runTrace initial i.actions }

end Conformance.InferenceObservation
