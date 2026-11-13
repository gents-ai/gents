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

private def failureToken : EnrollmentActivationObservation.Failure → String
  | .authentication => "authentication"
  | .route => "route"

private def transportToken : Transport → String
  | .healthy => "healthy"
  | .degraded => "degraded"
  | .wedged => "wedged"

def stateToken : State → String
  | .healthy => "healthy"
  | .waiting => "waiting"
  | .blocked => "blocked"

def causeToken : Cause → String
  | .runtimeConnecting => "runtime_connecting"
  | .runtimeDisconnected => "runtime_disconnected"
  | .runtimeRefused => "runtime_refused"
  | .runtimeNoEndpoint => "runtime_no_endpoint"
  | .enrollmentAuthentication => "enrollment_authentication"
  | .enrollmentRoute => "enrollment_route"
  | .discoveryDegraded => "discovery_degraded"
  | .discoveryOffline => "discovery_offline"

def nextActionToken : NextAction → String
  | .wait => "wait"
  | .restoreAccess => "restore_access"
  | .connectRuntime => "connect_runtime"
  | .enrollAgain => "enroll_again"

private def statusOf : String → Option RuntimeConnectionObservation.Status
  | "connecting" => some .connecting
  | "connected" => some .connected
  | "disconnected" => some .disconnected
  | "refused" => some .refused
  | "noEndpoint" => some .noEndpoint
  | _ => none

private def failureOf : String → EnrollmentActivationObservation.Failure
  | "authentication" => .authentication
  | _ => .route

private def transportOf : String → Transport
  | "healthy" => .healthy
  | "degraded" => .degraded
  | _ => .wedged

structure FactsInput where
  runtimes : List (Option String)
  enrollment : List String
  transport : String
  configuredPeers : Nat
  dialedPeers : Nat
  deriving Lean.ToJson

structure ViewOutput where
  state : String
  cause : Option String
  nextAction : Option String
  runtimeCount : Nat
  connectedRuntimeCount : Nat
  deriving Lean.ToJson

private def factsInput (facts : Facts) : FactsInput :=
  { runtimes := facts.runtimes.map (·.map statusToken)
    enrollment := facts.enrollment.map failureToken
    transport := transportToken facts.discovery.transport
    configuredPeers := facts.discovery.configuredPeers
    dialedPeers := facts.discovery.dialedPeers }

private def factsOf (i : FactsInput) : Facts :=
  { runtimes := i.runtimes.map (·.bind statusOf)
    enrollment := i.enrollment.map failureOf
    discovery := ⟨transportOf i.transport, i.configuredPeers, i.dialedPeers⟩ }

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
