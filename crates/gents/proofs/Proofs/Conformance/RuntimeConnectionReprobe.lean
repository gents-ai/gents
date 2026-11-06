import Proofs.ClientShell.ObservationOrdering

namespace Conformance.RuntimeConnectionReprobe
open _root_.RuntimeConnectionObservation

def eventName : Event → String
  | .sourceStarted => "sourceStarted"
  | .subscriptionsReady => "subscriptionsReady"
  | .snapshotSucceeded => "snapshotSucceeded"
  | .transportFailed => "transportFailed"
  | .permissionDenied => "permissionDenied"

def casesJson : String :=
  let counts : List Nat := [0, 1, 2, 3, 4, 5, 6, 10, 31, 32, 63, 64, 100]
  let events : List Event := [.sourceStarted, .subscriptionsReady, .snapshotSucceeded,
    .transportFailed, .permissionDenied]
  let cases := counts.flatMap fun denials => events.map fun event =>
    "{\"denials\":" ++ toString denials ++ ",\"event\":\"" ++ eventName event ++
    "\",\"nextDenials\":" ++ toString (nextDenials denials event) ++
    ",\"delayMs\":" ++ toString (reprobeDelayMs denials) ++ "}"
  "[" ++ String.intercalate "," cases ++ "]"
end Conformance.RuntimeConnectionReprobe

def main : IO Unit := IO.println Conformance.RuntimeConnectionReprobe.casesJson
