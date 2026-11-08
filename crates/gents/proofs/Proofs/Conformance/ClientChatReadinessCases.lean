import Proofs.ClientShell.ChatReadiness
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.ChatReadinessCases

open _root_.ClientChatReadiness Conformance.Contracts

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def sourceJson : Source → String
  | .localStandard => "\"local-standard\""
  | .enrollment => "\"enrollment\""
  | .other => "null"

private def readinessJson : Readiness → String
  | .ready => "null"
  | .notReady cause => jsonString (causeToken cause)

private def routeJson : Option Route → String
  | none => "null"
  | some route =>
    "{\"source\":" ++ sourceJson route.source ++
    ",\"operator_endpoint\":" ++ boolJson route.operatorEndpoint ++
    ",\"pairing_ready\":" ++ boolJson route.pairingReady ++
    ",\"generation_complete\":" ++ boolJson route.generationComplete ++
    ",\"lease_expires_at_offset\":" ++
      (match route.leaseExpiresAt with | some offset => toString offset | none => "null") ++ "}"

/-- `refusal` is the cause token of the implemented contract
(`enrollmentFenceRequired`), or null for ready. `endpoint_suffices_refusal`
records the competing contract for the pending decision only. -/
private def caseJson (route : Option Route) : String :=
  "{\"route\":" ++ routeJson route ++
  ",\"now_offset\":" ++ toString caseNow ++
  ",\"refusal\":" ++ readinessJson (enrollmentFenceRequired route caseNow) ++
  ",\"endpoint_suffices_refusal\":" ++
    readinessJson (operatorEndpointSuffices route caseNow) ++ "}"

private def causeJson (cause : Cause) : String :=
  "{\"cause\":" ++ jsonString (causeToken cause) ++
  ",\"recovery_cause\":" ++
    (match recoveryAttempt (.notReady cause) with
     | .unknown (some recovered) => jsonString (causeToken recovered)
     | _ => "null") ++ "}"

def casesJson : String := jsonArray (caseRoutes.map caseJson)

def causesJson : String := jsonArray (allCauses.map causeJson)

end Conformance.ChatReadinessCases
