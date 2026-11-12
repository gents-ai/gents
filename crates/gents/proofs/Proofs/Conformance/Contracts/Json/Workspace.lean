import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Session.Workspace.Conformance
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.Contracts

open Conformance.ContractCases
open CommandPolicy
open Lean

def workspaceCaseJson (witness : Workspace.Conformance.WorkspaceCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"from\":" ++ jsonString witness.fromState.toDefraDB ++ ","
    ++ "\"to\":" ++ jsonString witness.toState.toDefraDB ++ ","
    ++ "\"seal_hash\":" ++ jsonOptionalString witness.sealHash ++ ","
    ++ "\"legal\":" ++ boolString witness.legal
    ++ "}"

def workspaceCasesJson : String :=
  jsonArray (Workspace.Conformance.workspaceCases.map workspaceCaseJson)

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
