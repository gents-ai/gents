import Proofs.Workspace.PathCapability
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.WorkspacePathCapabilityContracts
open Workspace.PathCapability Conformance.Contracts
private def boolString (b : Bool) := if b then "true" else "false"
private def cap : WorkspacePathCapability := .exactPaths ["src/main.rs", "src/new.rs"]
private def workspace : IsolatedWorkspace :=
  {workspaceId := "workspace-1", workUnitId := "work-1", repositoryId := "repo-1",
   baseSha := "immutable-base", branch := "workspace-branch", creationPolicy := .gitWorktreeDiff,
   ownerDeploymentId := "host-1", sealHash := none, state := .ready, pathCapability := cap}
private def ready : Snapshot := ⟨workspace,none,none,0⟩
private def bind : Binding := binding workspace "tree-1"
private def evidence : Evidence := ⟨bind,[⟨"src/main.rs",.regular,true⟩],true,true,false,true,true⟩
private def sealed : Snapshot :=
  {ready with workspace := {workspace with state := .sealed, sealHash := some "tree-1"}, writer := some bind}
private def integrated : Snapshot := {sealed with integrator := some bind, trunkEffects := 1}
private def provisioning : Snapshot := {ready with workspace := {workspace with state := .provisioning}}
private def legacy : Snapshot :=
  {provisioning with workspace := {provisioning.workspace with pathCapability := .unrestrictedCompatibility}}
private def legacyEvidence : Evidence :=
  {evidence with expected := binding legacy.workspace "tree-1"}
private def empty : Snapshot := {ready with workspace := {workspace with pathCapability := .exactPaths []}}
private def emptyEvidence : Evidence := {evidence with expected := binding empty.workspace "tree-1", delta := []}
private def emptySealed : Snapshot :=
  {empty with workspace := {empty.workspace with state := .sealed, sealHash := some "tree-1"}, writer := some emptyEvidence.expected}

structure Case where
  name : String
  before : Snapshot
  operation : Operation
  evidence : Evidence
  expected : Snapshot
  disposition : Disposition
  deriving DecidableEq, Repr

def cases : List Case :=
  [ ⟨"fresh_exact_provisions", provisioning,.provision,evidence,ready,.accepted⟩
  , ⟨"fresh_legacy_cannot_provision",legacy,.provision,legacyEvidence,legacy,.denied⟩
  , ⟨"existing_identical_legacy_recovers",legacy,.provision,
      {legacyEvidence with existingIdentityMatches := true},legacy,.recovered⟩
  , ⟨"legacy_recovery_missing_checkout_cannot_reprovision",legacy,.provision,
      {legacyEvidence with existingIdentityMatches := true, checkoutPresent := false},legacy,.denied⟩
  , ⟨"malformed_manifest_denied",provisioning,.provision,
      {evidence with manifestCanonical := false},provisioning,.denied⟩
  , ⟨"owned_tracked_edit_seals",ready,.seal,evidence,sealed,.accepted⟩
  , ⟨"owned_untracked_addition_seals",ready,.seal,
      {evidence with delta := [⟨"src/new.rs",.regular,true⟩]},sealed,.accepted⟩
  , ⟨"empty_exact_empty_delta_seals",empty,.seal,emptyEvidence,emptySealed,.accepted⟩
  , ⟨"empty_exact_rejects_changes",empty,.seal,
      {emptyEvidence with delta := evidence.delta},empty,.denied⟩
  , ⟨"unowned_build_log_rejected",ready,.seal,
      {evidence with delta := [⟨".tmp-build/test-build.log",.regular,true⟩]},ready,.denied⟩
  , ⟨"rename_unowned_destination_rejected",ready,.seal,
      {evidence with delta := [⟨"src/main.rs",.regular,true⟩,⟨"outside.rs",.regular,true⟩]},ready,.denied⟩
  , ⟨"rename_both_owned_accepted",ready,.seal,
      {evidence with delta := [⟨"src/main.rs",.regular,true⟩,⟨"src/new.rs",.regular,true⟩]},sealed,.accepted⟩
  , ⟨"changed_symlink_rejected",ready,.seal,
      {evidence with delta := [⟨"src/main.rs",.symlink,true⟩]},ready,.denied⟩
  , ⟨"changed_gitlink_rejected",ready,.seal,
      {evidence with delta := [⟨"src/main.rs",.gitlink,true⟩]},ready,.denied⟩
  , ⟨"noncanonical_delta_rejected",ready,.seal,
      {evidence with delta := [⟨"../outside",.regular,false⟩]},ready,.denied⟩
  , ⟨"mutable_head_cannot_replace_immutable_base",ready,.seal,
      {evidence with capturedBaseMatches := false},ready,.denied⟩
  , ⟨"snapshot_drift_cannot_apply_other_bytes",sealed,.integrate,
      {evidence with appliedSnapshotMatches := false},sealed,.denied⟩
  , ⟨"sealed_repair_rechecks_actual_delta",sealed,.seal,
      {evidence with delta := [⟨"outside.rs",.regular,true⟩]},sealed,.denied⟩
  , ⟨"authorized_integration_once",sealed,.integrate,evidence,integrated,.accepted⟩
  , ⟨"integration_rejects_unowned_delta",sealed,.integrate,
      {evidence with delta := [⟨"outside.rs",.regular,true⟩]},sealed,.denied⟩
  , ⟨"receipt_replay_without_checkout",integrated,.replayIntegrate,
      {evidence with checkoutPresent := false, appliedSnapshotMatches := false},integrated,.recovered⟩
  , ⟨"writer_receipt_replay_without_checkout",sealed,.replaySeal,
      {evidence with checkoutPresent := false},sealed,.recovered⟩
  , ⟨"changed_capability_cannot_replay_receipt",integrated,.replayIntegrate,
      {evidence with expected := {bind with capability := .unrestrictedCompatibility}},integrated,.denied⟩
  , ⟨"different_seal_cannot_integrate",sealed,.integrate,
      {evidence with expected := {bind with tree := "tree-2"}},sealed,.denied⟩
  ]

theorem cases_replay : ∀ c ∈ cases,
    execute c.before c.operation c.evidence = (c.expected,c.disposition) := by decide

private def capJson : WorkspacePathCapability → String
  | .unrestrictedCompatibility => "{\"mode\":\"unrestrictedCompatibility\"}"
  | .exactPaths paths => "{\"mode\":\"exactPaths\",\"paths\":" ++ jsonArray (paths.map jsonString) ++ "}"
private def bindingJson (b : Binding) :=
  "{\"workspace_id\":" ++ jsonString b.workspaceId ++ ",\"owner\":" ++ jsonString b.owner ++
  ",\"base\":" ++ jsonString b.base ++ ",\"capability\":" ++ capJson b.capability ++
  ",\"tree\":" ++ jsonString b.tree ++ "}"
private def receiptJson : Option Binding → String | none => "null" | some b => bindingJson b
private def snapshotJson (s : Snapshot) :=
  "{\"workspace_id\":" ++ jsonString s.workspace.workspaceId ++
  ",\"owner\":" ++ jsonString s.workspace.ownerDeploymentId ++
  ",\"base\":" ++ jsonString s.workspace.baseSha ++
  ",\"state\":" ++ jsonString s.workspace.state.toDefraDB ++
  ",\"capability\":" ++ capJson s.workspace.pathCapability ++
  ",\"seal\":" ++ jsonOptionalString s.workspace.sealHash ++
  ",\"writer\":" ++ receiptJson s.writer ++ ",\"integrator\":" ++ receiptJson s.integrator ++
  ",\"trunk_effects\":" ++ toString s.trunkEffects ++ "}"
private def kind : EntryKind → String | .regular => "regular" | .symlink => "symlink" | .gitlink => "gitlink"
private def changeJson (c : Change) :=
  "{\"path\":" ++ jsonString c.path ++ ",\"kind\":" ++ jsonString (kind c.kind) ++
  ",\"canonical\":" ++ boolString c.canonical ++ "}"
private def evidenceJson (e : Evidence) :=
  "{\"expected\":" ++ bindingJson e.expected ++ ",\"delta\":" ++ jsonArray (e.delta.map changeJson) ++
  ",\"manifest_canonical\":" ++ boolString e.manifestCanonical ++
  ",\"checkout_present\":" ++ boolString e.checkoutPresent ++
  ",\"existing_identity_matches\":" ++ boolString e.existingIdentityMatches ++
  ",\"captured_base_matches\":" ++ boolString e.capturedBaseMatches ++
  ",\"applied_snapshot_matches\":" ++ boolString e.appliedSnapshotMatches ++ "}"
private def operation : Operation → String
  | .provision => "provision" | .seal => "seal" | .integrate => "integrate"
  | .replaySeal => "replay_seal" | .replayIntegrate => "replay_integrate"
private def disposition : Disposition → String
  | .accepted => "accepted" | .recovered => "recovered" | .denied => "denied"
private def caseJson (c : Case) :=
  "{\"name\":" ++ jsonString c.name ++ ",\"before\":" ++ snapshotJson c.before ++
  ",\"operation\":" ++ jsonString (operation c.operation) ++ ",\"evidence\":" ++ evidenceJson c.evidence ++
  ",\"expected\":" ++ snapshotJson c.expected ++ ",\"disposition\":" ++ jsonString (disposition c.disposition) ++ "}"
def casesJson := jsonArray (cases.map caseJson)

structure MigrationCase where
  name : String
  legacySource : Bool
  stored : Option WorkspacePathCapability
  expected : Option WorkspacePathCapability
  deriving DecidableEq, Repr

def migrationCases : List MigrationCase :=
  [ ⟨"legacy_missing_explicitly_migrates",true,none,some .unrestrictedCompatibility⟩
  , ⟨"new_missing_stays_missing",false,none,none⟩
  , ⟨"legacy_injected_exact_overwritten",true,some cap,some .unrestrictedCompatibility⟩
  , ⟨"exact_capability_preserved",false,some cap,some cap⟩
  , ⟨"explicit_legacy_value_preserved",false,some .unrestrictedCompatibility,some .unrestrictedCompatibility⟩ ]
theorem migration_cases_replay : ∀ c ∈ migrationCases,
    migrateCapability c.legacySource c.stored = c.expected := by decide
private def optionalCapJson : Option WorkspacePathCapability → String
  | none => "null" | some cap => capJson cap
private def migrationCaseJson (c : MigrationCase) :=
  "{\"name\":" ++ jsonString c.name ++ ",\"legacy_source\":" ++ boolString c.legacySource ++
  ",\"stored\":" ++ optionalCapJson c.stored ++ ",\"expected\":" ++ optionalCapJson c.expected ++ "}"
def migrationCasesJson := jsonArray (migrationCases.map migrationCaseJson)

structure AliasCase where
  name : String
  changedPaths : List String
  basePaths : List String
  treePaths : List String
  changedPrefixes : List (String × String)
  basePrefixes : List (String × String)
  treePrefixes : List (String × String)
  expected : Bool
  deriving DecidableEq, Repr

def aliasCases : List AliasCase :=
  [ ⟨"unrelated_exotic_and_colliding_entries_unchanged",
      ["allowed.rs"], ["allowed.rs","odd:name","[draft]","Old/a","old/b"],
      ["allowed.rs","odd:name","[draft]","Old/a","old/b"],
      [("allowed.rs","allowed.rs")],
      [("allowed.rs","allowed.rs"),("odd:name","odd:name"),("[draft]","[draft]"),
       ("Old","old"),("Old/a","old/a"),("old","old"),("old/b","old/b")],
      [("allowed.rs","allowed.rs"),("odd:name","odd:name"),("[draft]","[draft]"),
       ("Old","old"),("Old/a","old/a"),("old","old"),("old/b","old/b")], true⟩
  , ⟨"changed_file_alias_denied", ["allowed.rs"], ["Allowed.rs"],
      ["Allowed.rs","allowed.rs"], [("allowed.rs","allowed.rs")],
      [("Allowed.rs","allowed.rs")],[("Allowed.rs","allowed.rs"),("allowed.rs","allowed.rs")], false⟩
  , ⟨"changed_directory_alias_denied", ["src/new.rs"], ["Src/other.rs"],
      ["Src/other.rs","src/new.rs"], [("src","src"),("src/new.rs","src/new.rs")],
      [("Src","src"),("Src/other.rs","src/other.rs")],
      [("Src","src"),("Src/other.rs","src/other.rs"),("src","src"),("src/new.rs","src/new.rs")], false⟩
  , ⟨"exact_shared_directory_allowed", ["src/new.rs"], ["src/other.rs"],
      ["src/other.rs","src/new.rs"], [("src","src"),("src/new.rs","src/new.rs")],
      [("src","src"),("src/other.rs","src/other.rs")],
      [("src","src"),("src/other.rs","src/other.rs"),("src","src"),("src/new.rs","src/new.rs")], true⟩
  , ⟨"empty_delta_ignores_existing_aliases", [], ["Old","old"], ["Old","old"], [],
      [("Old","old"),("old","old")], [("Old","old"),("old","old")], true⟩ ]

theorem alias_cases_replay : ∀ c ∈ aliasCases,
    (changedAliasesCanonical c.changedPrefixes c.basePrefixes &&
     changedAliasesCanonical c.changedPrefixes c.treePrefixes) = c.expected := by decide

private def pathsJson (paths : List String) := jsonArray (paths.map jsonString)
private def prefixesJson (prefixes : List (String × String)) :=
  jsonArray (prefixes.map fun p => jsonArray [jsonString p.1, jsonString p.2])
private def aliasCaseJson (c : AliasCase) :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"changed_paths\":" ++ pathsJson c.changedPaths ++
  ",\"base_paths\":" ++ pathsJson c.basePaths ++
  ",\"tree_paths\":" ++ pathsJson c.treePaths ++
  ",\"changed_prefixes\":" ++ prefixesJson c.changedPrefixes ++
  ",\"base_prefixes\":" ++ prefixesJson c.basePrefixes ++
  ",\"tree_prefixes\":" ++ prefixesJson c.treePrefixes ++
  ",\"expected\":" ++ boolString c.expected ++ "}"
def aliasCasesJson := jsonArray (aliasCases.map aliasCaseJson)

end Conformance.WorkspacePathCapabilityContracts
