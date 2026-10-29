import Proofs.Configuration
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.Contracts.PackInstallation

def finalityCasesJson : String := jsonArray (Configuration.packFinalityCases.map fun attempts =>
  let name := fun event : Configuration.PackAttemptEvidence => match event with
    | .noCommit => "no_commit" | .unknownCommit => "unknown_commit"
  "{\"attempts\":" ++ jsonStringArray (attempts.map name) ++
    ",\"unconfirmed\":" ++ toString (Configuration.packCommitUnconfirmed attempts) ++ "}")

def caseJson (name : String) (references : List (List String))
    (rootShips dependencyShips installed : List String) (isLocal : Bool := true) : String :=
  "{\"witness\":" ++ jsonString name ++
    ",\"references\":" ++ jsonArray (references.map jsonStringArray) ++
    ",\"root_ships\":" ++ jsonStringArray rootShips ++
    ",\"dependency_ships\":" ++ jsonStringArray dependencyShips ++
    ",\"installed\":" ++ jsonStringArray installed ++
    ",\"local\":" ++ toString isLocal ++
    ",\"removal_accepted\":" ++ toString (Configuration.pluginRemovalSafe
      references.flatten (rootShips ++ dependencyShips)) ++
    ",\"accepted\":" ++ toString (Configuration.pluginInstallAvailable
      references.flatten (rootShips ++ dependencyShips) installed isLocal) ++ "}"

def pinCaseJson (name : String) (source : String) (pin : Option String) (actual : String) : String :=
  "{\"name\":" ++ jsonString name ++ ",\"source\":" ++ jsonString source ++
    ",\"pin\":" ++ (match pin with | none => "null" | some value => jsonString value) ++
    ",\"actual\":" ++ jsonString actual ++
    ",\"accepted\":" ++ toString (Configuration.pluginPinMatches pin actual) ++ "}"

def pinCasesJson : String := jsonArray [
  pinCaseJson "installed_unpinned" "installed" none "a",
  pinCaseJson "installed_match" "installed" (some "a") "a",
  pinCaseJson "installed_mismatch" "installed" (some "b") "a",
  pinCaseJson "shipped_match" "shipped" (some "a") "a",
  pinCaseJson "shipped_mismatch" "shipped" (some "b") "a"
]

def wireLockCasesJson : String := jsonArray ([false, true].map fun held =>
  "{\"held\":" ++ toString held ++ ",\"publish_allowed\":" ++
    toString (Configuration.wireAndPluginPublishAllowed held) ++ "}")

def wireTargetCasesJson : String := jsonArray (
  [false, true].flatMap fun localTarget => [false, true].flatMap fun preview =>
    [false, true].map fun remove =>
      "{\"local\":" ++ toString localTarget ++ ",\"preview\":" ++ toString preview ++
      ",\"remove\":" ++ toString remove ++ ",\"allowed\":" ++
      toString (Configuration.wireTargetAllowed localTarget preview remove) ++ "}")

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
    [] ["acme/search"] [],
  caseJson "plugin_references.remote_empty" [[]] [] [] [] false,
  caseJson "plugin_references.remote_missing" [["acme/search"]] [] [] [] false,
  caseJson "plugin_references.remote_recorded" [["acme/search"]] [] [] ["acme/search"] false,
  caseJson "plugin_references.remote_shipped" [["acme/search"]] ["acme/search"] [] [] false,
  caseJson "plugin_references.remote_dependency" [[]] [] ["acme/search"] [] false
]

end Conformance.Contracts.PackInstallation
