import Proofs.ClientShell.Installation
import Lean

namespace Conformance.ClientInstallation
open Lean _root_.ClientInstallation

private def phaseJson (phase : Phase) : Json := toJson <| match phase with
  | .accepting => "accepting"
  | .draining => "draining"
  | .released => "released"

private def stateJson (state : State) : Json := Json.mkObj
  [("phase", phaseJson state.phase), ("history", toJson state.history),
   ("attachments", toJson state.observation.attachments),
   ("requested", toJson state.observation.requested)]

private def actionJson : Action → Json
  | .attach => Json.mkObj [("kind", toJson "attach")]
  | .detach => Json.mkObj [("kind", toJson "detach")]
  | .retire => Json.mkObj [("kind", toJson "retire")]
  | .release => Json.mkObj [("kind", toJson "release")]
  | .replace reserved => Json.mkObj [("kind", toJson "replace"), ("startupReserved", toJson reserved)]

private def traceJson (initial : State) (actions : List Action) : Json := Id.run do
  let mut state := initial
  let mut trace := []
  for action in actions do
    let result := step state action
    state := result.getD state
    trace := trace ++ [Json.mkObj [("action", actionJson action),
      ("accepted", toJson result.isSome), ("state", stateJson state)]]
  return Json.mkObj [("initial", stateJson initial), ("trace", toJson trace)]

private def actions : List Action := [.attach, .detach, .retire, .release, .replace false, .replace true]

private def traces : List (List Action) :=
  actions.flatMap fun first => actions.flatMap fun second =>
    actions.map fun third => [first, second, third]

def casesJson : String :=
  let generated := [Phase.accepting, .draining, .released].flatMap fun phase =>
    (List.range 3).flatMap fun attachments =>
      traces.map fun trace => traceJson
        { phase, history := 11, observation := { requested := some 7, attachments } } trace
  let retirement := traceJson
    { phase := .accepting, history := 11, observation := { requested := some 7, attachments := 2 } }
    [.retire, .attach, .replace true, .detach, .release, .detach, .release, .replace false, .replace true]
  (toJson (retirement :: generated)).compress

end Conformance.ClientInstallation
