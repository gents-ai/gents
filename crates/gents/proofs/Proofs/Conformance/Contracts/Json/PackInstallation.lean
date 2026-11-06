import Proofs.Configuration
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.Contracts.PackInstallation

def evidenceCasesJson : String := jsonArray (Configuration.hostedPackEffectCases.map fun
    (actor, graph, plan) =>
  "{\"actor_matches\":" ++ toString actor ++
    ",\"graph_verified\":" ++ toString graph ++
    ",\"plan_approved\":" ++ toString plan ++
    ",\"allowed\":" ++ toString
      (Configuration.hostedPackEffectAllowed actor graph plan true) ++ "}")

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
    ",\"removal_accepted\":" ++ toString (Configuration.pluginRemovalObservedSafe
      references.flatten (rootShips ++ dependencyShips) false) ++
    ",\"retain_notification_record\":" ++ toString
      (Configuration.retainPluginStoreRecord rootShips false) ++
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
    toString (Configuration.wireAndPluginPublishAllowed held) ++
    ",\"resolve_allowed\":" ++ toString (Configuration.stagedPluginResolutionAllowed held) ++ "}")

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


def identityBindingJson (binding : Configuration.PackIdentity.Binding) : String :=
  "{\"slot\":" ++ jsonString binding.slot ++
    ",\"creation_nonce\":" ++ jsonString binding.creationNonce ++
    ",\"agent_did\":" ++ jsonString binding.agentDid ++ "}"

def identityBindingsJson (bindings : List Configuration.PackIdentity.Binding) : String :=
  jsonArray (bindings.map identityBindingJson)

def identityCasesJson : String := jsonArray (Configuration.PackIdentity.cases.map fun c =>
  let result := Configuration.PackIdentity.plan c.recorded c.requested c.provisioned
  "{\"name\":" ++ jsonString c.name ++
    ",\"recorded\":" ++ identityBindingsJson c.recorded ++
    ",\"requested\":" ++ jsonStringArray c.requested ++
    ",\"provisioned\":" ++ identityBindingsJson c.provisioned ++
    ",\"accepted\":" ++ toString result.isSome ++
    ",\"selected\":" ++ identityBindingsJson ((result.map (·.selected)).getD []) ++
    ",\"next_recorded\":" ++ identityBindingsJson ((result.map (·.recorded)).getD []) ++ "}")

def identityReferenceCasesJson : String := jsonArray (Configuration.PackIdentity.referenceCases.map fun c =>
  let result := Configuration.PackIdentity.resolveDependency c.declared c.available c.dependency c.slot
  "{\"name\":" ++ jsonString c.name ++
    ",\"declared\":" ++ jsonStringArray c.declared ++
    ",\"available\":" ++ jsonArray (c.available.map fun entry =>
      "{\"coordinate\":" ++ jsonString entry.coordinate ++
      ",\"bindings\":" ++ identityBindingsJson entry.bindings ++ "}") ++
    ",\"dependency\":" ++ jsonString c.dependency ++
    ",\"slot\":" ++ jsonString c.slot ++
    ",\"expected\":" ++ (match result with
      | none => "null" | some binding => identityBindingJson binding) ++ "}")

def evalSubjectCasesJson : String := jsonArray (Configuration.PackIdentity.evalSubjectCases.map fun c =>
  let result := Configuration.PackIdentity.resolveEvalSubject c.selected c.slot
  "{\"name\":" ++ jsonString c.name ++
    ",\"selected\":" ++ (match c.selected with
      | none => "null" | some binding => identityBindingJson binding) ++
    ",\"slot\":" ++ jsonString c.slot ++
    ",\"expected\":" ++ (match result with
      | none => "null" | some did => jsonString did) ++ "}")

end Conformance.Contracts.PackInstallation
