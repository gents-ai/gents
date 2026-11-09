import Proofs.ClientShell.ChatReadiness
import Proofs.ClientShell.RoutePairing
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

private def readinessJson : Readiness → String
  | .ready => "null"
  | .notReady cause => jsonString (causeToken cause)

private def routeFieldsJson (spelling : String) (route : Route) : String :=
  "{\"source\":" ++ spelling ++
  ",\"operator_endpoint\":" ++ boolJson route.operatorEndpoint ++ "}"

/-- Each route with each native spelling of its source. -/
private def spelledRoutes (route : Option Route) : List (String × Option Route) :=
  match route with
  | none => [("null", none)]
  | some r => (sourceSpellings r.source).map fun spelling => (routeFieldsJson spelling r, some r)

private def fenceCaseJson (encoded : String × Option Route) : String :=
  let (routeJson, route) := encoded
  "{\"route\":" ++ routeJson ++ ",\"refusal\":" ++ readinessJson (chatReadiness route) ++ "}"

private def legToken : ClientRoutePairing.Leg → String
  | .ready => "ready"
  | .drifted => "drifted"
  | .unavailable => "unavailable"

private def legCaseJson (outbound inbound : ClientRoutePairing.Leg) : String :=
  "{\"outbound\":" ++ jsonString (legToken outbound) ++
  ",\"inbound\":" ++ jsonString (legToken inbound) ++
  ",\"combined\":" ++ jsonOptionalBool (ClientRoutePairing.combinedLegs outbound inbound) ++ "}"

private def causeJson (cause : Cause) : String :=
  "{\"cause\":" ++ jsonString (causeToken cause) ++
  ",\"next_action\":" ++ jsonString (nextActionToken (nextAction cause)) ++ "}"

def casesJson : String := jsonArray ((fenceRoutes.flatMap spelledRoutes).map fenceCaseJson)

def legsJson : String :=
  jsonArray (ClientRoutePairing.allLegs.flatMap fun outbound =>
    ClientRoutePairing.allLegs.map (legCaseJson outbound))

def causesJson : String := jsonArray (allCauses.map causeJson)

end Conformance.ChatReadinessCases
