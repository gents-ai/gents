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
  [false, true].map fun agentFree => Json.mkObj [
    ("declared", toJson declared), ("optional", toJson optional), ("agent_free", toJson agentFree),
    ("expected", toJson (pluginModelSlotAllowed declared optional agentFree))]

section Network
open ToolPolicy.PluginNetwork

private def renderV4 (ip : V4) : String := s!"{ip.a}.{ip.b}.{ip.c}.{ip.d}"

private def renderV6 (ip : V6) : String :=
  ":".intercalate <| [ip.s0, ip.s1, ip.s2, ip.s3, ip.s4, ip.s5, ip.s6, ip.s7].map
    fun n => String.mk (Nat.toDigits 16 n)

private def renderAddr : Addr → String
  | .v4 ip => renderV4 ip
  | .v6 ip => renderV6 ip

private def renderHost : Host → String
  | .name n => n
  | .ip (.v4 ip) => renderV4 ip
  | .ip (.v6 ip) => "[" ++ renderV6 ip ++ "]"

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

private def v4 (a b c d : Nat) : Addr := .v4 ⟨a, b, c, d⟩
private def v6 (s0 s1 s2 s3 s4 s5 s6 s7 : Nat) : Addr := .v6 ⟨s0, s1, s2, s3, s4, s5, s6, s7⟩
private def publicIp : Addr := v4 93 184 216 34
private def publicIps : List Addr :=
  [publicIp, v6 0x2606 0x4700 0 0 0 0 0 0x1111, v6 0 0 0 0 0 0xffff 0x5db8 0xd822,
   v6 0x2002 0x5db8 0xd822 0 0 0 0 0]
private def loopback6 : Addr := v6 0 0 0 0 0 0 0 1
private def internalIps : List Addr :=
  [v4 127 0 0 1, v4 10 1 2 3, v4 172 20 0 1, v4 192 168 1 1, v4 169 254 169 254,
   v4 100 64 0 1, v4 0 0 0 0, v4 198 18 0 1, v4 224 0 0 1, v4 255 255 255 255,
   loopback6, v6 0 0 0 0 0 0 0 0, v6 0 0 0 0 0 0xffff 0x7f00 1, v6 0xfe80 0 0 0 0 0 0 1,
   v6 0xfc00 0 0 0 0 0 0 1, v6 0xfd12 0 0 0 0 0 0 1, v6 0xff02 0 0 0 0 0 0 1,
   v6 0x2001 0xdb8 0 0 0 0 0 1, v6 0x2001 0 0 0 0 0 0 1, v6 0x2001 0x10 0 0 0 0 0 1,
   v6 0x2001 0x2f 0 0 0 0 0 1, v6 0x2002 0x7f00 1 0 0 0 0 0, v6 0x64 0xff9b 0 0 0 0 0x7f00 1]

private def grants : List Grant :=
  [ .sealed, .anyHost
  , .hosts [⟨.exact (.name "api.example.com"), false, none⟩]
  , .hosts [⟨.suffix "example.com", false, none⟩]
  , .hosts [⟨.exact (.name "api.example.com"), true, some 8080⟩]
  , .hosts [⟨.exact (.ip (v4 127 0 0 1)), true, some 8080⟩]
  , .hosts [⟨.exact (.ip loopback6), true, some 8080⟩]
  , .hosts [⟨.exact (.ip (v4 169 254 169 254)), true, none⟩, ⟨.exact (.name "example.com"), false, none⟩] ]

/-- Hostname targets pair with each resolution shape; an IP literal resolves
to itself. -/
private def targets : List (Target × List Addr) :=
  let names := ["api.example.com", "example.com", "deep.api.example.com", "other.test"]
  let shapes := [[publicIp], [], [publicIp, v4 10 0 0 5], [publicIp, loopback6]]
    ++ (publicIps ++ internalIps).map ([·])
  let named := names.flatMap fun n => [(true, 443), (false, 80), (true, 8080), (false, 8080)].flatMap
    fun (https, port) => shapes.map fun addrs => (Target.mk https (.name n) port, addrs)
  let literal := (publicIps ++ internalIps).flatMap fun ip =>
    [(true, 443), (false, 80), (false, 8080)].map fun (https, port) =>
      (Target.mk https (.ip ip) port, [ip])
  named ++ literal

private def renderGrant : Grant → Json
  | .sealed => Json.null
  | .anyHost => Json.str "any"
  | .hosts es => toJson (es.map renderEntry)

private def networkJson : Json := toJson <| grants.flatMap fun g =>
  targets.map fun (t, addrs) => Json.mkObj [
    ("grant", renderGrant g), ("url", toJson (renderTarget t)),
    ("addresses", toJson (addrs.map renderAddr)),
    ("public", toJson (addrs.map ipPublic)),
    ("expected", toJson (allowed g t addrs))]

end Network

private def callAccessJson : Json := toJson <| [PluginAccess.read, .readWrite].flatMap fun declared =>
  [false, true].flatMap fun writeFields =>
  [false, true].flatMap fun setsWriteField =>
  [none, some PluginAccess.read, some .readWrite].map fun granted => Json.mkObj [
    ("declared", toJson declared.spelling), ("write_fields", toJson writeFields),
    ("sets_write_field", toJson setsWriteField),
    ("granted", match granted with | none => Json.null | some access => toJson access.spelling),
    ("valid", toJson (pluginBindingValid declared writeFields)),
    ("call", toJson (pluginCallAccess declared writeFields setsWriteField).spelling),
    ("admitted", toJson (pluginCallAdmitted declared granted writeFields setsWriteField))]

def casesJson : String := (Json.mkObj [("consent", consentJson), ("budgets", budgetsJson),
  ("model_slots", modelSlotsJson), ("network", networkJson),
  ("call_access", callAccessJson)]).compress
end Conformance.PluginResources
