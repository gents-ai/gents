import Proofs.CanonicalOutput.Execution.Examples
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.DelegatedChildContracts

open CanonicalOutput
open CanonicalOutput.Execution.Examples
open Conformance.Contracts

structure Case where
  name : String
  parentDepth : Nat
  parentAgent : Nat
  childAgent : Nat
  choice : Workspace.ChildChoice

def observedBind : Workspace.ObservedWorkspace :=
  ⟨70, 2, some 71, .ready, true⟩

def cases : List Case :=
  [ ⟨"remote_depth_two_inherit_at_bound", 2, 1, 2,
      .inherit observedParentWorkspace⟩
  , ⟨"remote_depth_three_rejects_child", Subagent.maxSubagentDepth, 1, 2,
      .inherit observedParentWorkspace⟩
  , ⟨"readonly_parent_bind_readwrite_attenuates", 2, 1, 2,
      .bind observedBind (some .readWrite)⟩
  , ⟨"readonly_parent_provision_distinct_identity", 2, 1, 2,
      .provision observedProvisionedWorkspace true⟩
  , ⟨"provision_requires_verified_workspace", 2, 1, 2,
      .provision { observedProvisionedWorkspace with available := false } true⟩
  , ⟨"provision_requires_exact_parent_path", 2, 1, 2,
      .provision observedProvisionedWorkspace false⟩
  , ⟨"inherit_rejects_changed_seal", 2, 1, 2,
      .inherit { observedParentWorkspace with sealHash := some 99 }⟩
  ]

def expected (value : Case) : Option (Nat × Option DelegatedWorkspace) :=
  (acceptedDelegatedCallAtDepth value.parentDepth).bind fun row =>
    receiveDelegatedChild 1 2 8 value.parentAgent value.childAgent row value.choice

def stampJson (value : DelegatedWorkspace) : String :=
  "{\"workspace_id\":" ++ toString value.workspaceId ++
    ",\"workspace_owner_agent_did\":" ++ toString value.ownerAgent ++
    ",\"workspace_seal_hash\":" ++ jsonOptionalNat value.sealHash ++
    ",\"workspace_authority\":" ++ jsonString value.authority.toDefraDB ++ "}"

def observedJson (value : Workspace.ObservedWorkspace) : String :=
  "{\"workspace_id\":" ++ toString value.workspaceId ++
    ",\"workspace_owner_agent_did\":" ++ toString value.ownerAgent ++
    ",\"workspace_seal_hash\":" ++ jsonOptionalNat value.sealHash ++
    ",\"state\":" ++ jsonString value.state.toDefraDB ++
    ",\"available\":" ++ (if value.available then "true" else "false") ++ "}"

def choiceJson : Workspace.ChildChoice → String
  | .noWorkspace => "{\"kind\":\"none\"}"
  | .inherit workspace => "{\"kind\":\"inherit\",\"workspace\":" ++
      observedJson workspace ++ "}"
  | .bind workspace requested => "{\"kind\":\"bind\",\"workspace\":" ++
      observedJson workspace ++ ",\"requested_authority\":" ++
      (requested.map (jsonString ∘ BindingAuthority.toDefraDB)).getD "null" ++ "}"
  | .provision workspace parentPathExact =>
      "{\"kind\":\"provision\",\"workspace\":" ++ observedJson workspace ++
      ",\"parent_path_exact\":" ++
      (if parentPathExact then "true" else "false") ++ "}"

def caseJson (value : Case) : String :=
  let accepted := acceptedDelegatedCallAtDepth value.parentDepth
  let result := expected value
  "{\"name\":" ++ jsonString value.name ++
    ",\"parent_depth\":" ++ toString value.parentDepth ++
    ",\"parent_agent\":" ++ toString value.parentAgent ++
    ",\"child_agent\":" ++ toString value.childAgent ++
    ",\"delegated_input\":" ++ (accepted.map (fun row =>
      "{\"source_close_doc_id\":" ++ toString row.input.source.closeId ++
      ",\"source_stream\":" ++ toString row.input.source.stream ++
      ",\"arguments\":" ++ jsonString row.input.arguments ++
      ",\"parent_subagent_depth\":" ++ toString row.input.parentSubagentDepth ++ "}")).getD "null" ++
    ",\"parent_workspace\":" ++ ((accepted.bind (·.workspace)).map stampJson).getD "null" ++
    ",\"choice\":" ++ choiceJson value.choice ++
    ",\"expected\":" ++ (result.map (fun (depth, workspace) =>
      "{\"child_depth\":" ++ toString depth ++ ",\"child_workspace\":" ++
      (workspace.map stampJson).getD "null" ++ "}")).getD "null" ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

theorem generated_cases_have_accepted_source :
    cases.all (fun value => (acceptedDelegatedCallAtDepth value.parentDepth).isSome) = true := by
  native_decide

end Conformance.DelegatedChildContracts
