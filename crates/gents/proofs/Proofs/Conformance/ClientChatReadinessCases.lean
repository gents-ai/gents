import Proofs.ClientShell.ChatReadiness
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.ChatReadinessCases

open _root_.ClientChatReadiness Conformance.Contracts

private def boolJson (value : Bool) : String := if value then "true" else "false"

/-- Native spellings of a source. `other` is both a source-less and a manual
record. -/
private def sourceSpellings : Source → List String
  | .localStandard => ["\"local-standard\""]
  | .enrollment => ["\"enrollment\""]
  | .other => ["null", "\"manual\""]

private def leaseJson : Lease → String
  | .absent => "{\"kind\":\"absent\"}"
  | .malformed => "{\"kind\":\"malformed\"}"
  | .expiresAt instant => "{\"kind\":\"at\",\"offset\":" ++ toString instant ++ "}"

private def readinessJson : Readiness → String
  | .ready => "null"
  | .notReady cause => jsonString (causeToken cause)

private def routeFieldsJson (spelling : String) (route : Route) : String :=
  "{\"source\":" ++ spelling ++
  ",\"operator_endpoint\":" ++ boolJson route.operatorEndpoint ++
  ",\"managed\":" ++ boolJson route.managed ++
  ",\"pairing_ready\":" ++ boolJson route.pairingReady ++
  ",\"network_id\":" ++ boolJson route.networkId ++
  ",\"request_digest\":" ++ boolJson route.requestDigest ++
  ",\"sequence_positive\":" ++ boolJson route.sequencePositive ++
  ",\"lease\":" ++ leaseJson route.lease ++ "}"

/-- Each route with each native spelling of its source. -/
private def spelledRoutes (route : Option Route) : List (String × Option Route) :=
  match route with
  | none => [("null", none)]
  | some r => (sourceSpellings r.source).map fun spelling => (routeFieldsJson spelling r, some r)

private def fenceCaseJson (encoded : String × Option Route) : String :=
  let (routeJson, route) := encoded
  "{\"route\":" ++ routeJson ++
  ",\"now_offset\":" ++ toString caseNow ++
  ",\"refusal\":" ++ readinessJson (chatReadiness route) ++
  ",\"lease_fresh\":" ++
    (match route with
     | some r => boolJson (leaseFresh r.lease caseNow)
     | none => "null") ++ "}"

private def eventJson : Event → String
  | .restart => "{\"kind\":\"restart\"}"
  | .authorityFailure => "{\"kind\":\"authority_failure\"}"
  | .transportRotation => "{\"kind\":\"transport_rotation\"}"
  | .reconciled outcome => "{\"kind\":\"reconciled\",\"outcome\":" ++ jsonOptionalBool outcome ++ "}"
  | .generationRotation => "{\"kind\":\"generation_rotation\"}"
  | .endpointWritten operator => "{\"kind\":\"endpoint_written\",\"operator\":" ++ boolJson operator ++ "}"
  | .addDeployment operator => "{\"kind\":\"add_deployment\",\"operator\":" ++ boolJson operator ++ "}"

/-- The native enrollment writer applies a generation rotation only to an
enrollment route; other sources would be a conflict, not this transition. -/
private def transitionApplies : Event → Option Route → Bool
  | .generationRotation, some route => route.source == .enrollment
  | _, some _ => true
  | _, none => false

private def transitionCaseJson (event : Event) (encoded : String × Option Route) : String :=
  let (routeJson, route) := encoded
  let after := step event route
  "{\"route\":" ++ routeJson ++
  ",\"event\":" ++ eventJson event ++
  ",\"after_pairing_ready\":" ++
    (match after with | some r => boolJson r.pairingReady | none => "null") ++
  ",\"after_operator_endpoint\":" ++
    (match after with | some r => boolJson r.operatorEndpoint | none => "null") ++
  ",\"refusal_before\":" ++ readinessJson (chatReadiness route) ++
  ",\"refusal_after\":" ++ readinessJson (chatReadiness after) ++ "}"

private def legToken : Leg → String
  | .ready => "ready"
  | .drifted => "drifted"
  | .unavailable => "unavailable"

private def legCaseJson (outbound inbound : Leg) : String :=
  "{\"outbound\":" ++ jsonString (legToken outbound) ++
  ",\"inbound\":" ++ jsonString (legToken inbound) ++
  ",\"combined\":" ++ jsonOptionalBool (combinedLegs outbound inbound) ++ "}"

private def causeJson (cause : Cause) : String :=
  "{\"cause\":" ++ jsonString (causeToken cause) ++
  ",\"next_action\":" ++ jsonString (nextActionToken (nextAction cause)) ++
  ",\"recovery_cause\":" ++
    (match recoveryAttempt true (.notReady cause) with
     | .unknown recovered => jsonString (causeToken recovered)
     | _ => "null") ++
  ",\"settled_recovery_reads_fence\":" ++
    boolJson (recoveryAttempt false (.notReady cause) != .recorded) ++ "}"

def casesJson : String := jsonArray ((fenceRoutes.flatMap spelledRoutes).map fenceCaseJson)

def transitionsJson : String :=
  jsonArray (transitionEvents.flatMap fun event =>
    ((transitionRoutes.filter (transitionApplies event)).flatMap spelledRoutes).map
      (transitionCaseJson event))

def legsJson : String :=
  jsonArray (allLegs.flatMap fun outbound => allLegs.map (legCaseJson outbound))

def causesJson : String := jsonArray (allCauses.map causeJson)

end Conformance.ChatReadinessCases
