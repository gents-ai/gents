import Proofs.ToolPolicy.Configuration
import Proofs.ToolPolicy.PluginNetwork
import Lean

namespace Conformance.PluginResources
open ToolPolicy Lean

private def consentJson : Json := toJson <| [0, 1, 64, 1536, 4096, 4294967295].flatMap fun requested =>
  [0, 1, 64, 1536, 4096].flatMap fun previous =>
  [false, true].map fun consent => Json.mkObj [
    ("requested", toJson requested), ("previous", toJson previous), ("consent", toJson consent),
    ("expected", toJson (pluginResourceConsented requested previous consent))]

private def budgetsJson : Json := toJson <| [(64, 4096), (5, 900), (60, 900), (1, 4)].flatMap fun (baseline, ceiling) =>
  [0, 1, baseline, ceiling, ceiling + 1, 4294967295].map fun requested => Json.mkObj [
    ("baseline", toJson baseline), ("requested", toJson requested), ("ceiling", toJson ceiling),
    ("expected", toJson (effectivePluginResource baseline requested ceiling))]

private def modelSlotsJson : Json := toJson <| [false, true].flatMap fun declared =>
  [false, true].flatMap fun optional =>
  [false, true].map fun behaviorFree => Json.mkObj [
    ("declared", toJson declared), ("optional", toJson optional), ("behavior_free", toJson behaviorFree),
    ("expected", toJson (pluginModelSlotAllowed declared optional behaviorFree))]

section Network
open ToolPolicy.PluginNetwork

private def renderV4 (ip : V4) : String := s!"{ip.a}.{ip.b}.{ip.c}.{ip.d}"

private def renderHost : Host → String
  | .name n => n
  | .v4 ip => renderV4 ip

private def renderEntry (e : Entry) : String :=
  let scheme := if e.plaintext then "http://" else ""
  let host := match e.pattern with
    | .exact h => renderHost h
    | .suffix d => "*." ++ d
  let port := match e.port with
    | some p => s!":{p}"
    | none => ""
  scheme ++ host ++ port

private def renderTarget (t : Target) : String :=
  (if t.https then "https://" else "http://") ++ renderHost t.host ++ s!":{t.port}/"

private def publicIp : V4 := ⟨93, 184, 216, 34⟩
private def internalIps : List V4 :=
  [⟨127, 0, 0, 1⟩, ⟨10, 1, 2, 3⟩, ⟨172, 20, 0, 1⟩, ⟨192, 168, 1, 1⟩, ⟨169, 254, 169, 254⟩,
   ⟨100, 64, 0, 1⟩, ⟨0, 0, 0, 0⟩, ⟨198, 18, 0, 1⟩, ⟨224, 0, 0, 1⟩, ⟨255, 255, 255, 255⟩]

private def grants : List Grant :=
  [ .sealed, .anyHost
  , .hosts [⟨.exact (.name "api.example.com"), false, none⟩]
  , .hosts [⟨.suffix "example.com", false, none⟩]
  , .hosts [⟨.exact (.name "api.example.com"), true, some 8080⟩]
  , .hosts [⟨.exact (.v4 ⟨127, 0, 0, 1⟩), true, some 8080⟩]
  , .hosts [⟨.exact (.v4 ⟨169, 254, 169, 254⟩), true, none⟩, ⟨.exact (.name "example.com"), false, none⟩] ]

/-- Hostname targets pair with each resolution shape; an IP literal resolves
to itself. -/
private def targets : List (Target × List V4) :=
  let names := ["api.example.com", "example.com", "deep.api.example.com", "other.test"]
  let shapes := [[publicIp], [], [publicIp, ⟨10, 0, 0, 5⟩]] ++ internalIps.map ([·])
  let named := names.flatMap fun n => [(true, 443), (false, 80), (true, 8080), (false, 8080)].flatMap
    fun (https, port) => shapes.map fun addrs => (Target.mk https (.name n) port, addrs)
  let literal := (publicIp :: internalIps).flatMap fun ip =>
    [(true, 443), (false, 80), (false, 8080)].map fun (https, port) =>
      (Target.mk https (.v4 ip) port, [ip])
  named ++ literal

private def renderGrant : Grant → Json
  | .sealed => Json.null
  | .anyHost => Json.str "any"
  | .hosts es => toJson (es.map renderEntry)

private def networkJson : Json := toJson <| grants.flatMap fun g =>
  targets.map fun (t, addrs) => Json.mkObj [
    ("grant", renderGrant g), ("url", toJson (renderTarget t)),
    ("addresses", toJson (addrs.map renderV4)),
    ("public", toJson (addrs.map v4Public)),
    ("expected", toJson (allowed g t addrs))]

end Network

def casesJson : String := (Json.mkObj [("consent", consentJson), ("budgets", budgetsJson), ("model_slots", modelSlotsJson),
  ("network", networkJson)]).compress
end Conformance.PluginResources
