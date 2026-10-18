import Proofs.ClientShell.ObservationScope

namespace Conformance.ClientObservationScope

open _root_.ClientObservationScope

private def optionJson : Option Nat → String
  | none => "null"
  | some node => toString node

private def stateJson (state : State) : String :=
  "{\"requested\":" ++ optionJson state.requested ++
  ",\"attachments\":" ++ toString state.attachments ++
  ",\"effective\":" ++ optionJson (effective state) ++ "}"

private def actionJson : Action → String
  | .select node => "{\"kind\":\"select\",\"node\":" ++ optionJson node ++ "}"
  | .attach => "{\"kind\":\"attach\"}"
  | .detach => "{\"kind\":\"detach\"}"

private def traceJson (initial : State) (actions : List Action) : String := Id.run do
  let mut state := initial
  let mut states := [state]
  for action in actions do
    state := step state action
    states := states ++ [state]
  return "{\"initial\":" ++ stateJson initial ++
    ",\"actions\":[" ++ String.intercalate "," (actions.map actionJson) ++
    "],\"expected\":[" ++ String.intercalate "," (states.map stateJson) ++ "]}"

def casesJson : String :=
  let nodes : List (Option Nat) := [none, some 1, some 2]
  let actions : List Action := [.select none, .select (some 1), .select (some 2), .attach, .detach]
  let traces := actions.flatMap fun first =>
    actions.flatMap fun second => actions.map fun third => [first, second, third]
  let cases := nodes.flatMap fun requested =>
    (List.range 3).flatMap fun attachments =>
      traces.map fun actions => traceJson { requested, attachments } actions
  "[" ++ String.intercalate "," cases ++ "]"

end Conformance.ClientObservationScope
