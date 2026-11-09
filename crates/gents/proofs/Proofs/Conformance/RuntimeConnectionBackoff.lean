import Proofs.ClientShell.ObservationOrdering

/-! Executes the reconnect schedule: for each consecutive reconnect count and
event, the next count (`nextAttempts`), whether the event reconnects, and the
delay of the reconnect it schedules (`reconnectDelayMs`). -/
namespace Conformance.RuntimeConnectionBackoff
open _root_.RuntimeConnectionObservation

def eventName : Event → String
  | .sourceStarted => "sourceStarted"
  | .subscriptionsReady => "subscriptionsReady"
  | .snapshotSucceeded => "snapshotSucceeded"
  | .transportFailed => "transportFailed"
  | .refused => "refused"
  | .renewed => "renewed"
  | .endpointMissing => "endpointMissing"

def casesJson : String :=
  let counts : List Nat := [0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 31, 32, 63, 64, 100]
  let events : List Event := [.sourceStarted, .subscriptionsReady, .snapshotSucceeded,
    .transportFailed, .refused, .renewed, .endpointMissing]
  let cases := counts.flatMap fun attempts => events.map fun event =>
    "{\"attempts\":" ++ toString attempts ++ ",\"event\":\"" ++ eventName event ++
    "\",\"reconnects\":" ++ toString (reconnects event) ++
    ",\"nextAttempts\":" ++ toString (nextAttempts attempts event) ++
    ",\"delayMs\":" ++ toString (reconnectDelayMs attempts) ++ "}"
  "[" ++ String.intercalate "," cases ++ "]"
end Conformance.RuntimeConnectionBackoff

def main : IO Unit := IO.println Conformance.RuntimeConnectionBackoff.casesJson
