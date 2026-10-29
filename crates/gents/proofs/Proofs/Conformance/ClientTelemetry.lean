import Proofs.ClientShell.Telemetry

namespace Conformance.ClientTelemetry
open _root_.ClientTelemetry

private def boolJson (value : Bool) : String := if value then "true" else "false"
private def stateJson (s : State) : String :=
  "{\"configured\":" ++ boolJson s.configured ++ ",\"consent\":" ++ boolJson s.consent ++
  ",\"persisted\":" ++ boolJson s.persisted ++ ",\"opened\":" ++ boolJson s.opened ++
  ",\"in_flight\":" ++ boolJson s.inFlight ++ "}"
private def actionJson : Action → String
  | .open => "{\"kind\":\"open\"}"
  | .enable ok disk => "{\"kind\":\"enable\",\"write_ok\":" ++ boolJson ok ++
      ",\"disk_after\":" ++ boolJson disk ++ "}"
  | .disable ok disk => "{\"kind\":\"disable\",\"write_ok\":" ++ boolJson ok ++
      ",\"disk_after\":" ++ boolJson disk ++ "}"
  | .restart => "{\"kind\":\"restart\"}"
private def traceJson (initial : State) (actions : List Action) : String := Id.run do
  let mut state := initial
  let mut expected : List String := []
  for action in actions do
    let (next, send) := step state action
    state := next
    expected := expected ++ ["{\"state\":" ++ stateJson state ++ ",\"send\":" ++ boolJson send ++ "}"]
  return "{\"initial\":" ++ stateJson initial ++ ",\"actions\":[" ++
    String.intercalate "," (actions.map actionJson) ++ "],\"expected\":[" ++
    String.intercalate "," expected ++ "]}"

def casesJson : String :=
  let actions : List (List Action) := [
    [.open, .enable true true, .open, .restart, .open, .open, .disable true false, .restart, .open],
    [.enable true true, .open, .disable true false, .restart, .open],
    [.open, .disable true false, .restart, .enable true true, .open],
    [.disable false false, .restart, .open],
    [.enable false false, .open],
    [.open, .enable true true]
  ]
  let cases := [false, true].flatMap fun configured =>
    [false, true].flatMap fun saved =>
      actions.map fun trace => traceJson ⟨configured, saved, saved, false, false⟩ trace
  "[" ++ String.intercalate "," cases ++ "]"
end Conformance.ClientTelemetry
