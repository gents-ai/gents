import Proofs.Clients.ClientShell.SyncHealth
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.ClientSyncHealthCases

open _root_.ClientSyncHealth Conformance.Contracts

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

private def caseJson (facts : Facts) : String :=
  let shown := view facts
  "{\"runtimes\":" ++ jsonArray (facts.runtimes.map fun status =>
      jsonOptionalString (status.map statusToken)) ++
  ",\"enrollment\":" ++ jsonArray (facts.enrollment.map fun failure =>
      jsonString (failureToken failure)) ++
  ",\"transport\":" ++ jsonString (transportToken facts.discovery.transport) ++
  ",\"configured_peers\":" ++ toString facts.discovery.configuredPeers ++
  ",\"dialed_peers\":" ++ toString facts.discovery.dialedPeers ++
  ",\"runtime_count\":" ++ toString (runtimeCount facts) ++
  ",\"connected_runtime_count\":" ++ toString (connectedRuntimeCount facts) ++
  ",\"state\":" ++ jsonString (stateToken shown.state) ++
  ",\"cause\":" ++ jsonOptionalString (shown.cause.map causeToken) ++
  ",\"next_action\":" ++ jsonOptionalString (shown.nextAction.map nextActionToken) ++ "}"

def casesJson : String := jsonArray (allFacts.map caseJson)

end Conformance.ClientSyncHealthCases
