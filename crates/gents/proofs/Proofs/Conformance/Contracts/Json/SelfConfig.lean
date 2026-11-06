import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.SelfConfig.Cases
import Proofs.SelfConfig.Auth
import Proofs.SelfConfig.Scope

namespace Conformance.Contracts

open SelfConfig SelfConfig.ContractCases

private def scBool (b : Bool) : String :=
  if b then "true" else "false"

def selfConfigFieldTableJson (t : Target) : String :=
  "{"
    ++ "\"collection\":" ++ jsonString t.collectionName ++ ","
    ++ "\"unique_field\":" ++ jsonString t.uniqueField ++ ","
    ++ "\"category\":" ++ jsonString t.category ++ ","
    ++ "\"all_fields\":" ++ jsonStringArray (allFields t) ++ ","
    ++ "\"writable_fields\":" ++ jsonStringArray (writableFields t) ++ ","
    ++ "\"protected_fields\":" ++ jsonStringArray (protectedFields t)
  ++ "}"

def selfConfigFieldTablesJson : String :=
  jsonArray (allTargets.map selfConfigFieldTableJson)

def selfConfigResourceReadExposureCasesJson : String :=
  let cases : List (String × Target × List String) :=
    [("backend", .inferenceBackend, ["backend"]),
     ("backend", .inferenceBackend, ["backend", "node"]),
     ("backend", .inferenceBackend, ["backend", "agent"]),
     ("backend", .inferenceBackend, ["node"]),
     ("backend", .inferenceBackend, ["agent"]),
     ("tools", .tools, ["tools"]),
     ("profile", .inferenceProfile, ["profile"]),
     ("automation", .task, ["automation"]),
     ("automation", .task, ["backend"]),
     ("effective", .agent, ["backend"]),
     ("effective", .agent, ["automation"]),
     ("effective", .agent, ["tools"])]
  jsonArray (cases.map fun (resource, target, enabled) =>
    "{\"resource\":" ++ jsonString resource ++
    ",\"categories\":" ++ jsonStringArray enabled ++
    ",\"exposed\":" ++ scBool (if resource == "effective" then effectiveReadExposed enabled
      else resourceReadExposed enabled target) ++ "}")

def fieldValuePairJson (entry : FieldKey × FieldValue) : String :=
  "{"
    ++ "\"field\":" ++ jsonString entry.1 ++ ","
    ++ "\"value\":" ++ jsonString entry.2
  ++ "}"

def selfConfigPatchEntryJson (entry : FieldKey × Option FieldValue) : String :=
  "{"
    ++ "\"field\":" ++ jsonString entry.1 ++ ","
    ++ "\"action\":"
      ++ (match entry.2 with
          | some _ => jsonString "set"
          | none => jsonString "clear") ++ ","
    ++ "\"value\":"
      ++ (match entry.2 with
          | some v => jsonString v
          | none => "null")
  ++ "}"

def selfConfigBackendJson (entry : String × String × String) : String :=
  "{"
    ++ "\"backend_id\":" ++ jsonString entry.1 ++ ","
    ++ "\"provider_kind\":" ++ jsonString entry.2.1 ++ ","
    ++ "\"auth\":" ++ jsonString entry.2.2
  ++ "}"

def selfConfigGrantsJson (g : Grants) : String :=
  "{\"pack_install\":" ++ scBool g.packInstall ++ "}"

def selfConfigSelectionJson : Option (List (FieldKey × FieldValue)) → String
  | none => "null"
  | some doc => jsonArray (doc.map fieldValuePairJson)

def selfConfigCaseJson (w : CaseWitness) : String :=
  "{"
    ++ "\"name\":" ++ jsonString w.row.name ++ ","
    ++ "\"collection\":" ++ jsonString w.row.target.collectionName ++ ","
    ++ "\"category\":" ++ jsonString w.row.target.category ++ ","
    ++ "\"guarded\":" ++ scBool w.row.guarded ++ ","
    ++ "\"validates\":" ++ scBool w.row.validates ++ ","
    ++ "\"held_grants\":" ++ selfConfigGrantsJson w.row.held ++ ","
    ++ "\"selected_before\":" ++ selfConfigSelectionJson w.selectedBefore ++ ","
    ++ "\"selected_after\":" ++ selfConfigSelectionJson w.selectedAfter ++ ","
    ++ "\"doc\":" ++ jsonArray (w.row.doc.map fieldValuePairJson) ++ ","
    ++ "\"patch\":"
      ++ jsonArray (w.row.patch.map selfConfigPatchEntryJson) ++ ","
    ++ "\"admissible\":" ++ scBool w.admissiblePatch ++ ","
    ++ "\"accepted\":" ++ scBool w.accepted ++ ","
    ++ "\"result\":" ++ jsonArray (w.result.map fieldValuePairJson) ++ ","
    ++ "\"protected_preserved\":" ++ scBool w.protectedPreserved ++ ","
    ++ "\"containment_holds\":" ++ scBool w.containmentHolds ++ ","
    ++ "\"unchanged_on_reject\":" ++ scBool w.unchangedOnReject ++ ","
    ++ "\"control_kept_after_accept\":" ++ scBool w.controlKeptAfterAccept ++ ","
    ++ "\"backends\":" ++ jsonArray (w.row.backends.map selfConfigBackendJson)
  ++ "}"

def selfConfigCasesJson : String :=
  jsonArray (selfConfigCases.map selfConfigCaseJson)

def schemaPublicationCasesJson : String := Id.run do
  let mut rows : List String := []
  for grant in [false, true] do
    for artifact in [false, true] do
      for compatible in [false, true] do
        for nativeAllowed in [false, true] do
          rows := rows ++ ["{\"grant\":" ++ scBool grant
            ++ ",\"artifact\":" ++ scBool artifact
            ++ ",\"compatible\":" ++ scBool compatible
            ++ ",\"native_allowed\":" ++ scBool nativeAllowed
            ++ ",\"accepted\":" ++ scBool (SelfConfig.schemaPublicationAllowed grant artifact compatible nativeAllowed) ++ "}"]
  return jsonArray rows

/-- Edit authority by category, scope and held self-config categories. -/
def selfConfigScopeEditCasesJson : String := Id.run do
  let categorySets : List (String × List String) :=
    [("none", []), ("agent-only", ["agent"]), ("tools-only", ["tools"]),
     ("catalog-only", ["node"]), ("catalog-and-agent", ["node", "agent"]),
     ("catalog-and-tools", ["node", "tools"]), ("agent-and-tools", ["agent", "tools"]),
     ("engineer", ["node", "agent", "tools", "profile", "backend", "mcp_service", "automation"])]
  let documents := ["agent", "tools", "profile", "backend", "automation"]
  let scopes : List (String × SelfConfig.Scope) := [("own", .own), ("foreign", .foreign)]
  let mut rows : List String := []
  for (setName, categories) in categorySets do
    for document in documents do
      for (scopeName, scope) in scopes do
        rows := rows ++ ["{\"name\":" ++ jsonString (setName ++ "/" ++ document ++ "/" ++ scopeName)
          ++ ",\"categories\":" ++ jsonStringArray categories
          ++ ",\"category\":" ++ jsonString document
          ++ ",\"scope\":" ++ jsonString scopeName
          ++ ",\"allowed\":" ++ scBool (SelfConfig.mayEdit categories document scope) ++ "}"]
  return jsonArray rows

def selfConfigScopeGroupCasesJson : String := Id.run do
  let groups : List (String × SelfConfig.SelfConfigGroup) :=
    [("created", SelfConfig.createdGroup), ("clone-of-engineer", SelfConfig.cloneGroup SelfConfig.engineerGroup),
     ("engineer", SelfConfig.engineerGroup), ("enabled-default", ⟨true, none⟩),
     ("disabled-with-list", ⟨false, some ["node", "tools"]⟩),
     ("enabled-catalog", ⟨true, some ["node"]⟩)]
  let rows := groups.map fun (name, g) =>
    "{\"name\":" ++ jsonString name
      ++ ",\"enabled\":" ++ scBool g.enabled
      ++ ",\"categories\":" ++ jsonOptionalStringArray g.categories
      ++ ",\"effective\":" ++ jsonStringArray (SelfConfig.effectiveCategories g)
      ++ ",\"may_create_agent\":" ++ scBool (SelfConfig.mayCreateAgent (SelfConfig.effectiveCategories g)) ++ "}"
  return jsonArray rows

def selfConfigScopeOfCasesJson : String := Id.run do
  let rows : List (String × String × String × Bool) :=
    [("self-exclusive", "agent-a", "agent-a", false), ("self-shared", "agent-a", "agent-a", true),
     ("other-exclusive", "agent-a", "agent-b", false), ("other-shared", "agent-a", "agent-b", true)]
  return jsonArray (rows.map fun (name, invoking, target, shared) =>
    "{\"name\":" ++ jsonString name
      ++ ",\"invoking_agent\":" ++ jsonString invoking
      ++ ",\"target_agent\":" ++ jsonString target
      ++ ",\"shared\":" ++ scBool shared
      ++ ",\"scope\":" ++ jsonString (if SelfConfig.scopeOf invoking target shared == .own then "own" else "foreign") ++ "}")

def selfConfigScopeCasesJson : String :=
  "{\"edit\":" ++ selfConfigScopeEditCasesJson
    ++ ",\"groups\":" ++ selfConfigScopeGroupCasesJson
    ++ ",\"scope_of\":" ++ selfConfigScopeOfCasesJson ++ "}"

end Conformance.Contracts
