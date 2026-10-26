import Proofs.RuntimeReconcile.ServiceReplacement
import Lean

namespace Conformance.RuntimeServiceReplacement
open Lean _root_.RuntimeReconcile.ServiceReplacement

private def phaseJson (phase : Phase) : Json := toJson <| match phase with
  | .accepting => "accepting"
  | .stopping => "stopping"
  | .released => "released"

private def stateJson (state : State) : Json := Json.mkObj
  [("phase", phaseJson state.phase), ("executions", toJson state.executions),
   ("nativeOutstanding", toJson state.nativeOutstanding)]

private def actionJson : Action → Json
  | .admit => Json.mkObj [("kind", toJson "admit")]
  | .finish => Json.mkObj [("kind", toJson "finish")]
  | .nativeStarted => Json.mkObj [("kind", toJson "nativeStarted")]
  | .nativeReturned => Json.mkObj [("kind", toJson "nativeReturned")]
  | .retire authority => Json.mkObj [("kind", toJson "retire"),
      ("operatorAuthenticated", toJson authority.operatorAuthenticated),
      ("observedBuildMatches", toJson authority.observedBuildMatches)]
  | .release released => Json.mkObj [("kind", toJson "release"),
      ("storeReleased", toJson released)]
  | .replace reserved => Json.mkObj [("kind", toJson "replace"),
      ("startupReserved", toJson reserved)]

private def traceJson (initial : State) (actions : List Action) : Json := Id.run do
  let mut state := initial
  let mut trace := []
  for action in actions do
    let result := step state action
    state := result.getD state
    trace := trace ++ [Json.mkObj [("action", actionJson action),
      ("accepted", toJson result.isSome), ("state", stateJson state)]]
  return Json.mkObj [("initial", stateJson initial), ("trace", toJson trace)]

private def retirements : List Action := [false, true].flatMap fun authenticated =>
  [false, true].map fun buildMatches => .retire ⟨authenticated, buildMatches⟩

private def actions : List Action :=
  [.admit, .finish, .nativeStarted, .nativeReturned,
   .release false, .release true, .replace false, .replace true] ++ retirements

def casesJson : String :=
  let generated := [Phase.accepting, .stopping, .released].flatMap fun phase =>
    (List.range 3).flatMap fun executions =>
      (List.range 3).flatMap fun nativeOutstanding =>
        actions.flatMap fun first => actions.map fun second =>
          traceJson { phase, executions, nativeOutstanding } [first, second]
  let retire := Action.retire ⟨true, true⟩
  let boundaries := [
    traceJson ⟨.accepting, 0, 0⟩
      [.admit, retire, .finish, retire, .admit, .release false,
       .nativeStarted, .release true, .nativeReturned, .release true,
       .replace false, .replace true],
    traceJson ⟨.accepting, 0, 0⟩
      [retire, retire, .admit, .release true, .replace true, .replace true]]
  (toJson (boundaries ++ generated)).compress

end Conformance.RuntimeServiceReplacement
