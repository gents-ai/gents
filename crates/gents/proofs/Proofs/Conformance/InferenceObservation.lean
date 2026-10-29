import Proofs.ClientShell.InferenceObservation

namespace Conformance.InferenceObservation
open ClientInferenceObservation

private def target : Target := ⟨"node", "session", "request", "generation"⟩
private def other : Target := ⟨"node", "other", "request", "generation"⟩
private def otherGeneration : Target := ⟨"node", "session", "request", "new-generation"⟩
private def initial : State := ⟨0, some target, false, none⟩

private def activityName : Activity → String
  | .awaitingProvider => "awaiting_provider"
  | .working => "working"
  | .retrying => "retrying"
  | .waiting => "waiting"
  | .unavailable => "unavailable"

private def optionActivityJson : Option Activity → String
  | none => "null"
  | some activity => "\"" ++ activityName activity ++ "\""

private def targetJson (target : Target) : String :=
  "{\"node\":\"" ++ target.node ++ "\",\"session\":\"" ++ target.session ++
  "\",\"request\":\"" ++ target.request ++ "\",\"generation\":\"" ++ target.generation ++ "\"}"

private def observationJson (o : Observation) : String :=
  "{\"target\":" ++ targetJson o.target ++ ",\"turn\":" ++ toString o.turn ++ ",\"attempt\":" ++ toString o.attempt ++
  ",\"sequence\":" ++ toString o.sequence ++ ",\"observed_at\":" ++ toString o.observedAt ++
  ",\"activity\":\"" ++ activityName o.activity ++ "\"}"

private inductive Action where
  | receive (epoch : Nat) (observation : Observation)
  | select (target : Option Target) (terminal : Bool)
  | project (now maxAge : Nat)

private def actionJson : Action → String
  | .receive epoch observation => "{\"kind\":\"receive\",\"epoch\":" ++ toString epoch ++
      ",\"observation\":" ++ observationJson observation ++ "}"
  | .select target terminal => "{\"kind\":\"select\",\"target\":" ++
      (match target with | none => "null" | some t => targetJson t) ++
      ",\"terminal\":" ++ (if terminal then "true" else "false") ++ "}"
  | .project now maxAge => "{\"kind\":\"project\",\"now\":" ++ toString now ++
      ",\"max_age\":" ++ toString maxAge ++ "}"

private def traceJson (actions : List Action) : String := Id.run do
  let mut state := initial
  let mut expected : List String := []
  for action in actions do
    match action with
    | .receive epoch observation => state := receive state epoch observation
    | .select target terminal => state := select state target terminal
    | .project _ _ => pure ()
    let projected := match action with
      | .project now maxAge => project state now maxAge
      | _ => state.observation.map (·.activity)
    expected := expected ++ [optionActivityJson projected]
  return "{\"actions\":[" ++ String.intercalate "," (actions.map actionJson) ++
    "],\"expected\":[" ++ String.intercalate "," expected ++ "]}"

private def obs (attempt sequence time : Nat) (activity : Activity) : Observation :=
  ⟨target, 0, attempt, sequence, time, activity⟩

def casesJson : String :=
  let traces : List (List Action) := [
    [.receive 0 (obs 0 1 10 .awaitingProvider), .receive 0 (obs 0 2 11 .retrying),
      .receive 0 (obs 1 3 12 .working), .project 15 5, .project 18 5],
    [.receive 0 (obs 1 4 10 .waiting), .receive 0 (obs 0 5 11 .working),
      .receive 0 (obs 1 3 12 .retrying), .project 9 5],
    [.receive 0 (obs 0 1 10 .working), .select (some other) false,
      .receive 0 (obs 0 2 11 .retrying), .select (some target) false,
      .receive 2 (obs 0 3 12 .waiting)],
    [.receive 0 (obs 0 1 10 .working), .select (some target) true,
      .receive 1 (obs 1 2 11 .retrying)],
    [.receive 0 (obs 0 1 10 .working), .select (some target) false,
      .receive 0 (obs 0 2 11 .working), .project 12 5],
    [.receive 0 (obs 2 1 10 .retrying),
      .receive 0 { (obs 0 2 11 .working) with turn := 1 },
      .receive 0 (obs 3 3 12 .retrying)],
    [.receive 0 (obs 0 1 10 .working),
      .receive 0 { (obs 0 2 11 .retrying) with target := otherGeneration },
      .select (some otherGeneration) false, .receive 1 (obs 1 3 12 .waiting)]
  ]
  "[" ++ String.intercalate "," (traces.map traceJson) ++ "]"
end Conformance.InferenceObservation
