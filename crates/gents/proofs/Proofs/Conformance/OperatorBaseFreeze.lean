import Proofs.Workspace.OperatorBaseFreeze
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.OperatorBaseFreezeContracts
open Workspace.PathCapability Workspace.PathCapability.OperatorBaseFreeze
open Conformance.Contracts

private def workspace : IsolatedWorkspace :=
  {workspaceId := "workspace-1", workUnitId := "work-1", repositoryId := "repo-1",
   baseSha := "base-commit", branch := "workspace-branch", creationPolicy := .gitWorktreeDiff,
   ownerDeploymentId := "host-1", sealHash := none, state := .ready,
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

/-- A frozen operator base creates no writer receipt, so the existing integration
owner still rejects it. No synthetic producing request is manufactured. -/
theorem frozen_base_not_writer_integrable :
    (Workspace.PathCapability.execute
      (OperatorBaseFreeze.execute ready evidence).1 .integrate
      ⟨binding workspace "base-tree",[],true,true,false,true,true⟩).2 = .denied := by decide

private def boolJson (b : Bool) := if b then "true" else "false"
private def dispositionString : Disposition → String
  | .accepted => "accepted" | .recovered => "recovered" | .denied => "denied"
private def capabilityJson : WorkspacePathCapability → String
  | .exactPaths paths => "{\"mode\":\"exactPaths\",\"paths\":" ++ jsonArray (paths.map jsonString) ++ "}"
  | .unrestrictedCompatibility => "{\"mode\":\"unrestrictedCompatibility\"}"
private def bindingJson (b : Binding) :=
  "{\"workspace_id\":" ++ jsonString b.workspaceId ++ ",\"owner\":" ++ jsonString b.owner ++
  ",\"base\":" ++ jsonString b.base ++ ",\"tree\":" ++ jsonString b.tree ++
  ",\"capability\":" ++ capabilityJson b.capability ++ "}"
private def caseJson (c : Case) :=
  let e := c.evidence
  "{\"name\":" ++ jsonString c.name ++
  ",\"before_state\":" ++ jsonString c.before.workspace.state.toDefraDB ++
  ",\"before_seal\":" ++ (c.before.workspace.sealHash.map jsonString).getD "null" ++
  ",\"capability\":" ++ capabilityJson c.before.workspace.pathCapability ++
  ",\"expected_binding\":" ++ bindingJson e.expected ++
  ",\"base_tree\":" ++ jsonString e.baseTree ++
  ",\"seal_capability\":" ++ boolJson e.sealCapability ++
  ",\"owner_and_placement_verified\":" ++ boolJson e.ownerAndPlacementVerified ++
  ",\"no_active_writer\":" ++ boolJson e.noActiveWriter ++
  ",\"checkout_present\":" ++ boolJson e.checkoutPresent ++
  ",\"manifest_canonical\":" ++ boolJson e.manifestCanonical ++
  ",\"captured_base_matches\":" ++ boolJson e.capturedBaseMatches ++
  ",\"changed_paths\":" ++ jsonArray (e.delta.map (jsonString ∘ Change.path)) ++
  ",\"expected_state\":" ++ jsonString c.expectedState.toDefraDB ++
  ",\"expected_disposition\":" ++ jsonString (dispositionString c.disposition) ++
  ",\"expected_writer_receipt\":false,\"expected_trunk_effects\":0}"
def casesJson := jsonArray (cases.map caseJson)
end Conformance.OperatorBaseFreezeContracts
