import Proofs.ClientShell.ObservationOrdering

namespace Conformance.RuntimeConnectionObservation
open _root_.RuntimeConnectionObservation

def statusName : Status → String
  | .connecting => "connecting"
  | .connected => "connected"
  | .disconnected => "disconnected"
  | .denied => "denied"

def eventName : Event → String
  | .sourceStarted => "sourceStarted"
  | .subscriptionsReady => "subscriptionsReady"
  | .snapshotSucceeded => "snapshotSucceeded"
  | .transportFailed => "transportFailed"
  | .permissionDenied => "permissionDenied"

def casesJson : String :=
  let states : List Status := [.connecting, .connected, .disconnected, .denied]
  let events : List Event := [.sourceStarted, .subscriptionsReady, .snapshotSucceeded,
    .transportFailed, .permissionDenied]
  let cases := states.flatMap fun state => events.map fun event =>
    "{\"status\":\"" ++ statusName state ++ "\",\"event\":\"" ++ eventName event ++
    "\",\"expected\":\"" ++ statusName (observe state event) ++
    "\",\"blocksAdmission\":" ++ toString (blocksAdmission state) ++ "}"
  "[" ++ String.intercalate "," cases ++ "]"
end Conformance.RuntimeConnectionObservation

def main : IO Unit := IO.println Conformance.RuntimeConnectionObservation.casesJson
