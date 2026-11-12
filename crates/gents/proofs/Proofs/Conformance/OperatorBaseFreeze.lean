import Proofs.Session.Workspace.OperatorBaseFreeze
import Proofs.Conformance.Family

namespace Conformance.OperatorBaseFreezeContracts
open Workspace.PathCapability Workspace.PathCapability.OperatorBaseFreeze

private def workspace : IsolatedWorkspace :=
  {workspaceId := "workspace-1", workUnitId := "work-1", repositoryId := "repo-1",
   baseSha := "base-commit", branch := "workspace-branch", creationPolicy := .gitWorktreeDiff,
   ownerNodeDid := "host-1", sealHash := none, state := .ready,
   pathCapability := .exactPaths []}
private def ready : Snapshot := ⟨workspace,none,none,0⟩
private def sealed : Snapshot :=
  {ready with workspace := {workspace with state := .sealed, sealHash := some "base-tree"}}
private def evidence : OperatorBaseFreeze.Evidence :=
  {expected := binding workspace "base-tree", baseTree := "base-tree",
   sealCapability := true, ownerAndPlacementVerified := true, noActiveWriter := true,
   checkoutPresent := true, manifestCanonical := true, capturedBaseMatches := true, delta := []}
private def writable : Snapshot :=
  {ready with workspace := {workspace with pathCapability := .exactPaths ["src/main.rs"]}}
private def legacy : Snapshot :=
  {ready with workspace := {workspace with pathCapability := .unrestrictedCompatibility}}

structure Case where
  name : String
  before : Snapshot
  evidence : OperatorBaseFreeze.Evidence
  expectedState : WorkspaceState
  disposition : Disposition
  deriving Repr

def cases : List Case :=
  [⟨"clean_empty_base_freezes",ready,evidence,.sealed,.accepted⟩
  ,⟨"missing_seal_capability_denied",ready,{evidence with sealCapability := false},.ready,.denied⟩
  ,⟨"wrong_owner_or_placement_denied",ready,{evidence with ownerAndPlacementVerified := false},.ready,.denied⟩
  ,⟨"nonempty_exact_capability_denied",writable,
      {evidence with expected := binding writable.workspace "base-tree"},.ready,.denied⟩
  ,⟨"legacy_capability_denied",legacy,
      {evidence with expected := binding legacy.workspace "base-tree"},.ready,.denied⟩
  ,⟨"active_writer_denied",ready,{evidence with noActiveWriter := false},.ready,.denied⟩
  ,⟨"dirty_delta_denied",ready,{evidence with delta := [⟨"changed.txt",.regular,true⟩]},.ready,.denied⟩
  ,⟨"changed_committed_head_denied",ready,{evidence with capturedBaseMatches := false},.ready,.denied⟩
  ,⟨"missing_checkout_denied",ready,{evidence with checkoutPresent := false},.ready,.denied⟩
  ,⟨"malformed_manifest_denied",ready,{evidence with manifestCanonical := false},.ready,.denied⟩
  ,⟨"wrong_identity_binding_denied",ready,
      {evidence with expected := {evidence.expected with base := "different-base"}},.ready,.denied⟩
  ,⟨"identical_seal_replay",sealed,evidence,.sealed,.recovered⟩
  ,⟨"identical_replay_without_checkout",sealed,{evidence with checkoutPresent := false},.sealed,.recovered⟩
  ,⟨"different_seal_replay_denied",{sealed with workspace := {sealed.workspace with sealHash := some "other-tree"}},
      evidence,.sealed,.denied⟩
  ,⟨"cleaning_replay_denied",{sealed with workspace := {sealed.workspace with state := .cleaning}},
      evidence,.cleaning,.denied⟩
  ,⟨"cleaned_replay_denied",{sealed with workspace := {sealed.workspace with state := .cleaned}},
      {evidence with checkoutPresent := false},.cleaned,.denied⟩]

theorem cases_replay : ∀ c ∈ cases,
    (OperatorBaseFreeze.execute c.before c.evidence).1.workspace.state = c.expectedState ∧
    (OperatorBaseFreeze.execute c.before c.evidence).2 = c.disposition := by decide

theorem frozen_base_not_writer_integrable :
    (Workspace.PathCapability.execute
      (OperatorBaseFreeze.execute ready evidence).1 .integrate
      ⟨binding workspace "base-tree",[],true,true,false,true,true⟩).2 = .denied := by decide

deriving instance Lean.ToJson for Disposition

structure CapabilityWire where
  mode : String
  paths : Option (List String)
  deriving Lean.ToJson

private def capabilityWire : WorkspacePathCapability → CapabilityWire
  | .exactPaths paths => { mode := "exactPaths", paths := some paths }
  | .unrestrictedCompatibility => { mode := "unrestrictedCompatibility", paths := none }

structure BindingWire where
  workspaceId : String
  owner : String
  base : String
  tree : String
  capability : CapabilityWire
  deriving Lean.ToJson

structure FreezeInput where
  name : String
  beforeState : String
  beforeSeal : Option String
  capability : CapabilityWire
  expectedBinding : BindingWire
  baseTree : String
  sealCapability : Bool
  ownerAndPlacementVerified : Bool
  noActiveWriter : Bool
  checkoutPresent : Bool
  manifestCanonical : Bool
  capturedBaseMatches : Bool
  changedPaths : List String
  deriving Lean.ToJson

structure FreezeOutput where
  state : String
  disposition : Disposition
  writerReceipt : Bool
  trunkEffects : Nat
  deriving Lean.ToJson

private def freezeInput (c : Case) : FreezeInput :=
  let e := c.evidence
  { name := c.name, beforeState := c.before.workspace.state.toDefraDB
    beforeSeal := c.before.workspace.sealHash
    capability := capabilityWire c.before.workspace.pathCapability
    expectedBinding :=
      { workspaceId := e.expected.workspaceId, owner := e.expected.owner, base := e.expected.base
        tree := e.expected.tree, capability := capabilityWire e.expected.capability }
    baseTree := e.baseTree, sealCapability := e.sealCapability
    ownerAndPlacementVerified := e.ownerAndPlacementVerified, noActiveWriter := e.noActiveWriter
    checkoutPresent := e.checkoutPresent, manifestCanonical := e.manifestCanonical
    capturedBaseMatches := e.capturedBaseMatches, changedPaths := e.delta.map Change.path }

private def freezeOutput (c : Case) : FreezeOutput :=
  let (after, disposition) := OperatorBaseFreeze.execute c.before c.evidence
  { state := after.workspace.state.toDefraDB, disposition
    writerReceipt := after.writer.isSome, trunkEffects := after.trunkEffects }

/-- The wire input names its case; the model executes the case's own snapshot
and evidence, whose capability carries arguments the wire cannot. -/
@[conformance_family]
def freeze : Family FreezeInput FreezeOutput where
  inputs := cases.map freezeInput
  model := fun i => match cases.find? (·.name == i.name) with
    | some c => freezeOutput c
    | none => { state := "", disposition := .denied, writerReceipt := false, trunkEffects := 0 }

end Conformance.OperatorBaseFreezeContracts
