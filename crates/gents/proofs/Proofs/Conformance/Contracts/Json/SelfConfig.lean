import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.SelfConfig.Cases
import Proofs.SelfConfig.Auth
import Proofs.Identity.GrantSets

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

def policyResourceExposureCasesJson : String :=
  let cases : List (List String) :=
    [[], ["policy"], ["agent", "tools", "profile"], ["node", "policy"],
     ["backend", "mcp_service", "automation", "policy"]]
  jsonArray (cases.map fun categories =>
    "{\"categories\":" ++ jsonStringArray categories ++
    ",\"exposed\":" ++ scBool (policyResourceExposed categories) ++ "}")

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
  "{\"pack_install\":" ++ scBool g.packInstall ++ ",\"policy\":" ++ scBool g.policy ++ "}"

/-- Every combination of the policy grant bit on the candidate, the stored Tools and
the invoker, with pack installation off. Rust `OperatorGrants::bounded_by` must
match `Grants.boundedBy` on each row. -/
def policyGrantBoundCasesJson : String :=
  let bools := [false, true]
  jsonArray <| bools.flatMap fun candidate => bools.flatMap fun stored =>
    bools.map fun held =>
      let c : Grants := { Grants.bot with policy := candidate }
      let s : Grants := { Grants.bot with policy := stored }
      let h : Grants := { Grants.bot with policy := held }
      "{\"candidate_policy\":" ++ scBool candidate
        ++ ",\"stored_policy\":" ++ scBool stored
        ++ ",\"held_policy\":" ++ scBool held
        ++ ",\"accepted\":" ++ scBool (c.boundedBy s h) ++ "}"

/-- The node permissions a no-lockout Agent keeps against its own revoke, and for
each permission and change, whether the lockout guard keeps it. Rust
`policy::keeps_lockout` must agree on every row. -/
def policyLockoutCasesJson : String :=
  let permissions := (Identity.GrantSets.lockoutProtected ++ ["patch-collection"]).eraseDups
  let rows := permissions.flatMap fun p =>
    [false, true].flatMap fun delete =>
      [false, true].flatMap fun noLockout =>
        [false, true].map fun isSelf =>
          let change : Identity.GrantSets.Change :=
            ⟨.permission p, delete, true, false⟩
          "{\"permission\":" ++ jsonString p
            ++ ",\"delete\":" ++ scBool delete
            ++ ",\"no_lockout\":" ++ scBool noLockout
            ++ ",\"is_self\":" ++ scBool isSelf
            ++ ",\"kept\":"
            ++ scBool (Identity.GrantSets.selfChangeKeepsLockout noLockout isSelf change) ++ "}"
  "{\"protected\":" ++ jsonStringArray Identity.GrantSets.lockoutProtected
    ++ ",\"rows\":" ++ jsonArray rows ++ "}"

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

end Conformance.Contracts
