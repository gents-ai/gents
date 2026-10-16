import Proofs.ToolPolicy.Configuration
import Lean

/-! Tools-document timeouts under host ceilings. Every expectation is computed by
the `ToolPolicy` owners; a `null` expectation is a document the owner rejects. -/

namespace Conformance.ToolTimeouts
open ToolPolicy Lean

private def pairJson : Option (Nat × Nat) → Json
  | none => Json.null
  | some (value, maximum) => Json.mkObj [("default", toJson value), ("maximum", toJson maximum)]

private def foregroundCases : List (String × Nat × Nat × Option Int × Option Int) := [
  ("unconfigured_uses_host_pair", 120, 120, none, none),
  ("unconfigured_keeps_decoupled_host_maximum", 120, 3600, none, none),
  ("authored_default_fixes_maximum", 120, 3600, some 60, none),
  ("authored_pair_within_host", 120, 3600, some 300, some 1800),
  ("authored_default_above_host_cap_clamped", 120, 120, some 600, none),
  ("authored_maximum_above_host_cap_clamped", 120, 600, some 60, some 3600),
  ("authored_maximum_only", 120, 3600, none, some 1800),
  ("host_default_narrowed_by_authored_maximum", 600, 600, none, some 300),
  ("authored_maximum_below_documented_default_rejected", 120, 3600, none, some 60),
  ("zero_default_rejected", 120, 120, some 0, none),
  ("maximum_below_default_rejected", 120, 3600, some 300, some 200)]

private def foregroundJson : Json := toJson (foregroundCases.map
  fun (name, hostDefault, hostMaximum, execution, maximum) =>
    Json.mkObj [("name", toJson name), ("host_default", toJson hostDefault),
      ("host_maximum", toJson hostMaximum), ("timeout_secs", toJson execution),
      ("max_timeout_secs", toJson maximum),
      ("expected", pairJson (effectiveBashForeground ⟨hostDefault, hostMaximum⟩
        ⟨execution, maximum, none, none, none⟩))])

private def cliCases : List (String × Nat × Nat × Nat × Option Int) := [
  ("unauthored_keeps_registration", 120, 120, 10, none),
  ("unauthored_keeps_registration_above_host_cap", 120, 120, 600, none),
  ("authored_raises_within_host_cap", 120, 3600, 10, some 300),
  ("authored_clamped_to_host_cap", 120, 120, 10, some 900),
  ("zero_rejected", 120, 120, 10, some 0)]

private def cliJson : Json := toJson (cliCases.map
  fun (name, hostDefault, hostMaximum, registration, authored) =>
    Json.mkObj [("name", toJson name), ("host_default", toJson hostDefault),
      ("host_maximum", toJson hostMaximum), ("registration_secs", toJson registration),
      ("timeout_secs", toJson authored),
      ("expected", toJson (effectiveCliTimeout ⟨hostDefault, hostMaximum⟩ registration authored))])

private def backgroundCases : List (String × Option Int) := [
  ("unconfigured_keeps_ceiling", none),
  ("authored_shortens_lifetime", some 60),
  ("authored_above_ceiling_clamped", some 86400),
  ("zero_rejected", some 0),
  ("negative_rejected", some (-5))]

private def backgroundJson : Json := toJson (backgroundCases.map fun (name, authored) =>
  Json.mkObj [("name", toJson name), ("background_timeout_secs", toJson authored),
    ("expected", toJson (effectiveBackgroundLifetime authored))])

/-- Requests probe the default, the lower bound, an in-range value and the maximum. -/
private def requests : List (Option Nat) := [none, some 0, some 1, some 45, some 5000]

private def boundedJson (name : String) (authored maximum : Option Int)
    (policy : Option (Nat × Nat)) (effective : Nat × Nat → Option Nat → Nat) : Json :=
  Json.mkObj [("name", toJson name), ("timeout_secs", toJson authored),
    ("max_timeout_secs", toJson maximum), ("expected", pairJson policy),
    ("requests", toJson (requests.map fun requested => Json.mkObj
      [("requested", toJson requested),
       ("effective", toJson (policy.map fun pair => effective pair requested))]))]

private def waitCases : List (String × Option Int × Option Int) := [
  ("unconfigured_keeps_fixed_defaults", none, none),
  ("authored_pair", some 5, some 20),
  ("authored_default_keeps_documented_maximum", some 60, none),
  ("authored_maximum_above_ceiling_clamped", none, some 1200),
  ("authored_pair_above_ceiling_clamped", some 900, some 1200),
  ("zero_rejected", some 0, none),
  ("maximum_below_documented_default_rejected", none, some 5)]

private def waitJson : Json := toJson (waitCases.map fun (name, wait, maximum) =>
  boundedJson name wait maximum
    ((resolvedRemoteWait wait maximum).map (underCeiling waitCeiling)) waitFor)

private def lspCases : List (String × Option Int × Option Int) := [
  ("unconfigured_keeps_fixed_defaults", none, none),
  ("authored_pair_below_request_floor", some 2, some 3),
  ("authored_default_keeps_documented_maximum", some 60, none),
  ("authored_maximum_above_ceiling_clamped", none, some 500),
  ("zero_rejected", some 0, none),
  ("maximum_below_documented_default_rejected", none, some 5)]

private def lspJson : Json := toJson (lspCases.map fun (name, timeout, maximum) =>
  boundedJson name timeout maximum
    ((resolvedLspTimeout timeout maximum).map (underCeiling lspActionCeiling)) lspActionFor)

def casesJson : String := (Json.mkObj
  [("foreground", foregroundJson), ("cli", cliJson), ("background", backgroundJson),
   ("wait", waitJson), ("lsp", lspJson)]).compress

example : effectiveBashForeground ⟨120, 3600⟩ ⟨some 60, none, none, none, none⟩ =
    some (60, 60) := by decide
example : effectiveBashForeground ⟨120, 120⟩ ⟨some 600, none, none, none, none⟩ =
    some (120, 120) := by decide
example : effectiveCliTimeout ⟨120, 120⟩ 10 (some 900) = some 120 := by decide
example : (resolvedRemoteWait (some 900) (some 1200)).map (underCeiling waitCeiling) =
    some (600, 600) := by decide

end Conformance.ToolTimeouts
