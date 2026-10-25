import Proofs.Configuration
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.Contracts.PackInstallation

def caseJson (name : String) (references : List (List String))
    (rootShips dependencyShips installed : List String) : String :=
  "{\"witness\":" ++ jsonString name ++
    ",\"references\":" ++ jsonArray (references.map jsonStringArray) ++
    ",\"root_ships\":" ++ jsonStringArray rootShips ++
    ",\"dependency_ships\":" ++ jsonStringArray dependencyShips ++
    ",\"installed\":" ++ jsonStringArray installed ++
    ",\"accepted\":" ++ toString (Configuration.pluginReferencesAvailable
      references.flatten (rootShips ++ dependencyShips) installed) ++ "}"

def casesJson : String := jsonArray [
  caseJson "plugin_references.empty" [[]] [] [] [],
  caseJson "plugin_references.missing" [["acme/search"]] [] [] [],
  caseJson "plugin_references.shipped" [["acme/search"]] ["acme/search"] [] [],
  caseJson "plugin_references.dependency" [["acme/search"]] [] ["acme/search"] [],
  caseJson "plugin_references.installed" [["acme/search"]] [] [] ["acme/search"],
  caseJson "plugin_references.wrong_namespace" [["other/search"]] ["acme/search"] [] [],
  caseJson "plugin_references.mixed" [["acme/search", "acme/parse"]]
    ["acme/search"] [] ["acme/parse"],
  caseJson "plugin_references.missing_in_dependency"
    [["acme/search"], ["acme/missing"]] ["acme/search"] [] [],
  caseJson "plugin_references.repeated" [["acme/search", "acme/search"]]
    [] ["acme/search"] []
]

end Conformance.Contracts.PackInstallation
