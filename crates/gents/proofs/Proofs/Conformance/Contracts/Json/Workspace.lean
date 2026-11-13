import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Session.Workspace.Conformance
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.Contracts

open Conformance.ContractCases
open CommandPolicy
open Lean

structure WorkspaceTransitionInput where
  name : String
  fromState : String
  toState : String
  sealHash : Option String
  deriving Lean.ToJson

structure WorkspaceTransitionOutput where
  legal : Bool
  state : String
  deriving Lean.ToJson

private def workspaceTransitionInput (c : Workspace.Conformance.WorkspaceCase) :
    WorkspaceTransitionInput :=
  { name := c.name, fromState := c.fromState.toDefraDB, toState := c.toState.toDefraDB
    sealHash := c.sealHash }

private def workspaceTransitionOf (i : WorkspaceTransitionInput) : WorkspaceTransitionOutput :=
  match WorkspaceState.fromDefraDB? i.fromState, WorkspaceState.fromDefraDB? i.toState with
  | some fromState, some toState =>
    let pre := Workspace.Conformance.preWorkspace
      { name := i.name, fromState, toState, sealHash := i.sealHash, legal := false }
    match IsolatedWorkspace.step? pre toState with
    | some post => { legal := true, state := post.state.toDefraDB }
    | none => { legal := false, state := pre.state.toDefraDB }
  | _, _ => { legal := false, state := i.fromState }

private def quickstartNames : List String := ["provision_frozen_base"]

/-- Quickstart provisioning is the production path that persists a workspace
straight from provisioning to sealed with the frozen base tree. -/
@[conformance_family]
def workspaceQuickstartTransition : Family WorkspaceTransitionInput WorkspaceTransitionOutput where
  inputs := (Workspace.Conformance.workspaceCases.filter (quickstartNames.contains ·.name)).map
    workspaceTransitionInput
  model := workspaceTransitionOf

/-- model-production-gap: production has no lifecycle-transition owner; each
transition is written inline by a host executor plan (create, seal, cleanup),
and no scenario replays these transitions through those plans. The illegal
transitions (sealing without a hash, skipping cleaning) have no production
path at all. -/
@[conformance_family]
def workspaceTransitionUnreached : Family WorkspaceTransitionInput WorkspaceTransitionOutput where
  inputs := (Workspace.Conformance.workspaceCases.filter (!quickstartNames.contains ·.name)).map
    workspaceTransitionInput
  model := workspaceTransitionOf

/-- The wire carries each workspace and binding enum as its DefraDB name, the
string production stores and compares. -/
private def ofWire {α : Type} (decode : String → Option α) (fallback : α) (value : String) : α :=
  (decode value).getD fallback

structure BindingWire where
  bindingId : String
  workspaceId : String
  requestId : String
  authority : String
  nodeDid : String
  sealHash : Option String
  state : String
  deriving ToJson

def BindingWire.ofWitness (workspaceId : String) (b : Workspace.Conformance.BindingWitness) :
    BindingWire :=
  { bindingId := b.bindingId, workspaceId, requestId := b.requestId
    authority := b.authority.toDefraDB, nodeDid := b.nodeDid, sealHash := b.sealHash
    state := b.state.toDefraDB }

def BindingWire.toWitness (b : BindingWire) : Workspace.Conformance.BindingWitness :=
  { bindingId := b.bindingId, requestId := b.requestId
    authority := ofWire BindingAuthority.fromDefraDB? .readOnly b.authority
    nodeDid := b.nodeDid, sealHash := b.sealHash
    state := ofWire BindingState.fromDefraDB? .denied b.state }

structure BindingInput where
  name : String
  workspaceId : String
  workspaceState : String
  workspaceSealHash : Option String
  ownerNodeDid : String
  creationPolicy : String
  existing : List BindingWire
  candidate : BindingWire
  gitMetadataWrite : Bool
  agentCommandMode : ExecutionMode
  deriving ToJson

def BindingInput.ofCase (c : Workspace.Conformance.WorkspaceBindingCase) : BindingInput :=
  { name := c.name, workspaceId := c.workspaceId
    workspaceState := c.workspaceState.toDefraDB
    workspaceSealHash := c.workspaceSealHash, ownerNodeDid := c.ownerNodeDid
    creationPolicy := c.creationPolicy.toDefraDB
    existing := c.existing.map (BindingWire.ofWitness c.workspaceId)
    candidate := BindingWire.ofWitness c.workspaceId c.candidate
    gitMetadataWrite := c.gitMetadataWrite, agentCommandMode := c.agentCommandMode }

def BindingInput.toCase (i : BindingInput) : Workspace.Conformance.WorkspaceBindingCase :=
  { name := i.name, workspaceId := i.workspaceId
    workspaceState := ofWire WorkspaceState.fromDefraDB? .provisionFailed i.workspaceState
    workspaceSealHash := i.workspaceSealHash, ownerNodeDid := i.ownerNodeDid
    creationPolicy := ofWire CreationPolicy.fromDefraDB? .gitWorktreeDiff i.creationPolicy
    existing := i.existing.map BindingWire.toWitness
    candidate := i.candidate.toWitness
    gitMetadataWrite := i.gitMetadataWrite, agentCommandMode := i.agentCommandMode
    legal := false }

/-- Premise: production binds a workspace under its owner node, so a binding's
owner is the workspace owner by construction; and git metadata writes are denied
by the spawn policy of the command, not by refusing the binding. Binding
admission therefore decides neither of these cases. -/
def bindingAdmissionUnreachedNames : List String :=
  [ "non_owner_node_cannot_claim"
  , "git_worktree_diff_read_write_git_metadata_write_illegal" ]

@[conformance_family]
def workspaceBindingAdmission : Family BindingInput Bool where
  inputs :=
    (Workspace.Conformance.workspaceBindingCases.filter fun c =>
      !bindingAdmissionUnreachedNames.contains c.name).map BindingInput.ofCase
  model := fun i => Workspace.Conformance.bindingCaseLegal i.toCase

/-- model-production-gap: the model refuses a binding whose node is not the
workspace owner and a read-write binding that would write git metadata on a
git-worktree-diff workspace; `admit_workspace_binding` takes neither the
workspace owner nor the command's git metadata writes, because production sets
the binding owner from the workspace and denies git metadata writes in the
command's spawn policy. -/
@[conformance_family]
def workspaceBindingAdmissionUnreached : Family BindingInput Bool where
  inputs :=
    (Workspace.Conformance.workspaceBindingCases.filter fun c =>
      bindingAdmissionUnreachedNames.contains c.name).map BindingInput.ofCase
  model := fun i => Workspace.Conformance.bindingCaseLegal i.toCase

end Conformance.Contracts
