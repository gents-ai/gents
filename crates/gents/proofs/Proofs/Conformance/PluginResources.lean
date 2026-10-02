import Proofs.ToolPolicy.Configuration
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

def casesJson : String := (Json.mkObj [("consent", consentJson), ("budgets", budgetsJson), ("model_slots", modelSlotsJson)]).compress
end Conformance.PluginResources
