import Proofs.Clients.ClientShell.ObservationOrdering

/-! Executes the reconnect schedule: for each origin and event whether it
reconnects, and for each consecutive reconnect count and connected duration the
count a reconnect is scheduled with (`reconnectAttempt`) and its delay
(`reconnectDelayMs`). -/
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

def originName : Origin → String
  | .source => "source"
  | .snapshot => "snapshot"

def optionNat : Option Nat → String
  | none => "null"
  | some n => toString n

def reconnectsJson : String :=
  let events : List Event := [.sourceStarted, .subscriptionsReady, .snapshotSucceeded,
    .transportFailed, .refused, .renewed, .endpointMissing]
  let cases := [Origin.source, .snapshot].flatMap fun origin => events.map fun event =>
    "{\"origin\":\"" ++ originName origin ++ "\",\"event\":\"" ++ eventName event ++
    "\",\"reconnects\":" ++ toString (reconnects origin event) ++ "}"
  "[" ++ String.intercalate "," cases ++ "]"

def scheduleJson : String :=
  let counts : List Nat := [0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 31, 32, 63, 64, 100]
  let connected : List (Option Nat) :=
    [none, some 0, some 250, some 29999, some 30000, some 30001, some 600000]
  let cases := counts.flatMap fun attempts => connected.map fun connectedMs =>
    let attempt := reconnectAttempt attempts connectedMs
    "{\"attempts\":" ++ toString attempts ++ ",\"connectedMs\":" ++ optionNat connectedMs ++
    ",\"attempt\":" ++ toString attempt ++
    ",\"delayMs\":" ++ toString (reconnectDelayMs attempt) ++ "}"
  "[" ++ String.intercalate "," cases ++ "]"

def casesJson : String :=
  "{\"reconnects\":" ++ reconnectsJson ++ ",\"schedule\":" ++ scheduleJson ++
  ",\"stableMs\":" ++ toString reconnectStableMs ++ "}"
end Conformance.RuntimeConnectionBackoff

def main : IO Unit := IO.println Conformance.RuntimeConnectionBackoff.casesJson
