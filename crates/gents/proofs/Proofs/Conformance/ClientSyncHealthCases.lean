import Proofs.Clients.ClientShell.SyncHealth
import Proofs.Conformance.Family

namespace Conformance.ClientSyncHealthCases

open _root_.ClientSyncHealth

/-- The native `RuntimeConnectionStatus::as_str` spelling. -/
private def statusToken : RuntimeConnectionObservation.Status → String
  | .connecting => "connecting"
  | .connected => "connected"
  | .disconnected => "disconnected"
  | .refused => "refused"
  | .noEndpoint => "noEndpoint"

def stateToken : State → String
  | .healthy => "healthy"
  | .waiting => "waiting"
  | .blocked => "blocked"

def causeToken : Cause → String
  | .runtimeConnecting => "runtime_connecting"
  | .runtimeDisconnected => "runtime_disconnected"
  | .runtimeRefused => "runtime_refused"
  | .runtimeNoEndpoint => "runtime_no_endpoint"

def nextActionToken : NextAction → String
  | .wait => "wait"
  | .restoreAccess => "restore_access"
  | .connectRuntime => "connect_runtime"

private def statusOf : String → Option RuntimeConnectionObservation.Status
  | "connecting" => some .connecting
  | "connected" => some .connected
  | "disconnected" => some .disconnected
  | "refused" => some .refused
  | "noEndpoint" => some .noEndpoint
  | _ => none

structure FactsInput where
  runtimes : List (Option String)
  deriving Lean.ToJson

structure ViewOutput where
  state : String
  cause : Option String
  nextAction : Option String
  runtimeCount : Nat
  connectedRuntimeCount : Nat
  deriving Lean.ToJson

private def factsInput (facts : Facts) : FactsInput :=
  { runtimes := facts.runtimes.map (·.map statusToken) }

private def factsOf (i : FactsInput) : Facts :=
  { runtimes := i.runtimes.map (·.bind statusOf) }

@[conformance_family]
def projection : Family FactsInput ViewOutput where
  inputs := allFacts.map factsInput
  model := fun i =>
    let facts := factsOf i
    let shown := view facts
    { state := stateToken shown.state
      cause := shown.cause.map causeToken
      nextAction := shown.nextAction.map nextActionToken
      runtimeCount := _root_.ClientSyncHealth.runtimeCount facts
      connectedRuntimeCount := _root_.ClientSyncHealth.connectedRuntimeCount facts }

end Conformance.ClientSyncHealthCases
