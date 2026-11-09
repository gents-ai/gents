import Proofs.ClientShell.ObservationOrdering

namespace Conformance.RuntimeConnectionObservation
open _root_.RuntimeConnectionObservation

def statusName : Status → String
  | .connecting => "connecting"
  | .connected => "connected"
  | .disconnected => "disconnected"
  | .denied => "denied"
  | .noEndpoint => "noEndpoint"

def eventName : Event → String
  | .sourceStarted => "sourceStarted"
  | .subscriptionsReady => "subscriptionsReady"
  | .snapshotSucceeded => "snapshotSucceeded"
  | .transportFailed => "transportFailed"
  | .permissionDenied => "permissionDenied"
  | .renewed => "renewed"
  | .endpointMissing => "endpointMissing"

def toneName : Tone → String
  | .ready => "ready"
  | .waiting => "waiting"
  | .blocked => "blocked"

def reasonName : Reason → String
  | .connected => "connected"
  | .connecting => "connecting"
  | .disconnected => "disconnected"
  | .permissionDenied => "permission_denied"
  | .noEndpoint => "no_endpoint"

def streamEndName : StreamEnd → String
  | .renewalDue => "renewalDue"
  | .completed => "completed"
  | .failed true => "deniedFailure"
  | .failed false => "transportFailure"

def optionJson : Option String → String
  | none => "null"
  | some value => "\"" ++ value ++ "\""

def statuses : List Status := [.connecting, .connected, .disconnected, .denied, .noEndpoint]

def events : List Event := [.sourceStarted, .subscriptionsReady, .snapshotSucceeded,
  .transportFailed, .permissionDenied, .renewed, .endpointMissing]

def transitionsJson : String :=
  let cases := statuses.flatMap fun state => events.map fun event =>
    "{\"status\":\"" ++ statusName state ++ "\",\"event\":\"" ++ eventName event ++
    "\",\"expected\":\"" ++ statusName (observe state event) ++
    "\",\"blocksAdmission\":" ++ toString (blocksAdmission state) ++ "}"
  "[" ++ String.intercalate "," cases ++ "]"

def presentationsJson : String :=
  let states : List (Option Status) := none :: statuses.map some
  let cases := states.map fun state =>
    let (tone, reason) := presentation state
    "{\"status\":" ++ optionJson (state.map statusName) ++ ",\"kind\":\"" ++ toneName tone ++
    "\",\"reason\":\"" ++ reasonName reason ++ "\"}"
  "[" ++ String.intercalate "," cases ++ "]"

def streamEndsJson : String :=
  let ends : List StreamEnd := [.renewalDue, .completed, .failed true, .failed false]
  let cases := ends.map fun streamEnd =>
    "{\"end\":\"" ++ streamEndName streamEnd ++ "\",\"event\":" ++
    optionJson ((streamEndEvent streamEnd).map eventName) ++ "}"
  "[" ++ String.intercalate "," cases ++ "]"

def snapshotOutcomeName : SnapshotOutcome → String
  | .succeeded => "succeeded"
  | .refused => "refused"
  | .rootWithheld => "rootWithheld"
  | .unreachable => "unreachable"

def armedEventsJson : String :=
  let cases := [false, true].map fun renewal =>
    "{\"renewal\":" ++ toString renewal ++ ",\"event\":\"" ++ eventName (armedEvent renewal) ++ "\"}"
  "[" ++ String.intercalate "," cases ++ "]"

def armedJson : String :=
  let cases := [false, true].flatMap fun wasArmed => events.map fun event =>
    "{\"wasArmed\":" ++ toString wasArmed ++ ",\"event\":\"" ++ eventName event ++
    "\",\"armed\":" ++ toString (armedAfter wasArmed event) ++ "}"
  "[" ++ String.intercalate "," cases ++ "]"

def snapshotOutcomesJson : String :=
  let outcomes : List SnapshotOutcome := [.succeeded, .refused, .rootWithheld, .unreachable]
  let cases := outcomes.map fun outcome =>
    "{\"outcome\":\"" ++ snapshotOutcomeName outcome ++ "\",\"event\":\"" ++
    eventName (snapshotEvent outcome) ++ "\"}"
  "[" ++ String.intercalate "," cases ++ "]"

def renewalStartsJson : String :=
  let cases := [(0, 0), (0, 1), (1000, 29999), (5000, 30000), (0, 45000), (7, 120000)].map
    fun (started, arm) =>
      "{\"startedMs\":" ++ toString started ++ ",\"armMs\":" ++ toString arm ++
      ",\"nextMs\":" ++ toString (renewalStartMs started arm) ++ "}"
  "[" ++ String.intercalate "," cases ++ "]"

/-- `cases` keeps its original shape (one row per status and event); the
presentation, stream-end, notice, snapshot-outcome, renewal and retry rows are
additional keys. -/
def casesJson : String :=
  "{\"cases\":" ++ transitionsJson ++ ",\"presentations\":" ++ presentationsJson ++
  ",\"streamEnds\":" ++ streamEndsJson ++
  ",\"armedEvents\":" ++ armedEventsJson ++ ",\"armed\":" ++ armedJson ++
  ",\"snapshotOutcomes\":" ++ snapshotOutcomesJson ++
  ",\"renewalStarts\":" ++ renewalStartsJson ++
  ",\"renewalPeriodMs\":" ++ toString renewalPeriodMs ++
  ",\"snapshotRetryDelayMs\":" ++ toString snapshotRetryDelayMs ++
  ",\"transportRetryDelayMs\":" ++ toString transportRetryDelayMs ++ "}"
end Conformance.RuntimeConnectionObservation

def main : IO Unit := IO.println Conformance.RuntimeConnectionObservation.casesJson
