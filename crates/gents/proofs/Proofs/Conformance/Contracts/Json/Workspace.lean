import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Session.Workspace.Conformance
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases
open CommandPolicy

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

def bindingWitnessJson (workspaceId : String)
    (witness : Workspace.Conformance.BindingWitness) : String :=
  "{"
    ++ "\"binding_id\":" ++ jsonString witness.bindingId ++ ","
    ++ "\"workspace_id\":" ++ jsonString workspaceId ++ ","
    ++ "\"request_id\":" ++ jsonString witness.requestId ++ ","
    ++ "\"authority\":" ++ jsonString witness.authority.toDefraDB ++ ","
    ++ "\"node_did\":" ++ jsonString witness.nodeDid ++ ","
    ++ "\"seal_hash\":" ++ jsonOptionalString witness.sealHash ++ ","
    ++ "\"state\":" ++ jsonString witness.state.toDefraDB
    ++ "}"

def workspaceBindingCaseJson
    (witness : Workspace.Conformance.WorkspaceBindingCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"workspace_id\":" ++ jsonString witness.workspaceId ++ ","
    ++ "\"workspace_state\":" ++ jsonString witness.workspaceState.toDefraDB ++ ","
    ++ "\"workspace_seal_hash\":" ++ jsonOptionalString witness.workspaceSealHash ++ ","
    ++ "\"owner_node_did\":" ++ jsonString witness.ownerNodeDid ++ ","
    ++ "\"creation_policy\":" ++ jsonString witness.creationPolicy.toDefraDB ++ ","
    ++ "\"existing\":"
      ++ jsonArray
        (witness.existing.map (bindingWitnessJson witness.workspaceId)) ++ ","
    ++ "\"candidate\":"
      ++ bindingWitnessJson witness.workspaceId witness.candidate ++ ","
    ++ "\"git_metadata_write\":" ++ boolString witness.gitMetadataWrite ++ ","
    ++ "\"agent_command_mode\":"
      ++ jsonString witness.agentCommandMode.toDefraDB ++ ","
    ++ "\"legal\":" ++ boolString witness.legal
    ++ "}"

def workspaceBindingCasesJson : String :=
  jsonArray
    (Workspace.Conformance.workspaceBindingCases.map workspaceBindingCaseJson)

end Conformance.Contracts
