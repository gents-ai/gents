import Proofs.Session.Workspace.PathCapability
import Proofs.Conformance.OperatorBaseFreeze
import Proofs.Conformance.Family

namespace Conformance.WorkspacePathCapabilityContracts
open Workspace.PathCapability
private def cap : WorkspacePathCapability := .exactPaths ["src/main.rs", "src/new.rs"]
private def workspace : IsolatedWorkspace :=
  {workspaceId := "workspace-1", workUnitId := "work-1", repositoryId := "repo-1",
   baseSha := "immutable-base", branch := "workspace-branch", creationPolicy := .gitWorktreeDiff,
   ownerNodeDid := "host-1", sealHash := none, state := .ready, pathCapability := cap}
private def ready : Snapshot := ⟨workspace,none,none,0⟩
private def bind : Binding := binding workspace "tree-1"
private def evidence : Evidence := ⟨bind,[⟨"src/main.rs",.regular,true⟩],true,true,false,true,true,[]⟩
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

private def operationWire : Operation → String
  | .provision => "provision" | .seal => "seal" | .integrate => "integrate"
  | .replaySeal => "replay_seal" | .replayIntegrate => "replay_integrate"

private def capabilityWire :
    WorkspacePathCapability → Conformance.OperatorBaseFreezeContracts.CapabilityWire
  | .exactPaths paths => { mode := "exactPaths", paths := some paths }
  | .unrestrictedCompatibility => { mode := "unrestrictedCompatibility", paths := none }

structure CaseInput where
  name : String
  operation : String
  state : String
  capability : Conformance.OperatorBaseFreezeContracts.CapabilityWire
  checkoutPresent : Bool
  manifestCanonical : Bool
  existingIdentityMatches : Bool
  deriving Lean.ToJson

private def caseInput (c : Case) : CaseInput :=
  { name := c.name, operation := operationWire c.operation
    state := c.before.workspace.state.toDefraDB
    capability := capabilityWire c.before.workspace.pathCapability
    checkoutPresent := c.evidence.checkoutPresent
    manifestCanonical := c.evidence.manifestCanonical
    existingIdentityMatches := c.evidence.existingIdentityMatches }

structure Observation where
  state : String
  disposition : Disposition
  writerReceipt : Bool
  integratorReceipt : Bool
  trunkEffects : Nat
  deriving Lean.ToJson

structure Outcome where
  disposition : Disposition
  deriving Lean.ToJson

private def observe (c : Case) : Observation :=
  let (after, disposition) := execute c.before c.operation c.evidence
  { state := after.workspace.state.toDefraDB, disposition
    writerReceipt := after.writer.isSome, integratorReceipt := after.integrator.isSome
    trunkEffects := after.trunkEffects - c.before.trunkEffects }

private def observeCase (i : CaseInput) : Observation :=
  match cases.find? (·.name == i.name) with
  | some c => observe c
  | none => { state := "", disposition := .denied, writerReceipt := false,
              integratorReceipt := false, trunkEffects := 0 }

private def outcomeCase (i : CaseInput) : Outcome :=
  { disposition := (observeCase i).disposition }

/-- The noncanonical delta is a raw Git tree entry (`../outside`) that a
checkout cannot hold, so it is driven through the tree-delta validator rather
than a seal of a working checkout. -/
def noncanonicalCases : List Case := cases.filter (·.name == "noncanonical_delta_rejected")

/-- The wire input names its case; the model executes the case's own snapshot
and evidence, whose capability and bindings carry arguments the wire cannot. -/
@[conformance_family]
def provision : Family CaseInput Outcome where
  inputs := (cases.filter (·.operation == .provision)).map caseInput
  model := outcomeCase

@[conformance_family]
def sealing : Family CaseInput Observation where
  inputs := (cases.filter fun c => c.operation == .seal && !noncanonicalCases.contains c).map caseInput
  model := observeCase

@[conformance_family]
def treeDelta : Family CaseInput Outcome where
  inputs := noncanonicalCases.map caseInput
  model := outcomeCase

@[conformance_family]
def integrate : Family CaseInput Observation where
  inputs := (cases.filter (·.operation == .integrate)).map caseInput
  model := observeCase

@[conformance_family]
def replay : Family CaseInput Observation where
  inputs := (cases.filter fun c => c.operation == .replaySeal || c.operation == .replayIntegrate).map caseInput
  model := observeCase

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

end Conformance.WorkspacePathCapabilityContracts

namespace Conformance.WorkspacePathAlias

open Lean Workspace.PathCapability

structure AliasInput where
  name : String
  changedPaths : List String
  basePaths : List String
  treePaths : List String
  changedPrefixes : List (List String)
  basePrefixes : List (List String)
  treePrefixes : List (List String)
  deriving ToJson

private def toPair (p : String × String) : List String := [p.1, p.2]
private def pairs (prefixes : List (List String)) : List (String × String) :=
  prefixes.map fun p => (p.headD "", p.getD 1 "")

def admitted (i : AliasInput) : Bool :=
  changedAliasesCanonical (pairs i.changedPrefixes) (pairs i.basePrefixes) &&
    changedAliasesCanonical (pairs i.changedPrefixes) (pairs i.treePrefixes)

@[conformance_family]
def admission : Family AliasInput Bool where
  inputs := Conformance.WorkspacePathCapabilityContracts.aliasCases.map fun c =>
    { name := c.name, changedPaths := c.changedPaths, basePaths := c.basePaths,
      treePaths := c.treePaths, changedPrefixes := c.changedPrefixes.map toPair,
      basePrefixes := c.basePrefixes.map toPair, treePrefixes := c.treePrefixes.map toPair }
  model := admitted

end Conformance.WorkspacePathAlias
