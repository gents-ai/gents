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
  sourceWorkspace : DelegatedWorkspace
  choice : Workspace.ChildChoice

def mkCase (name : String) (parentDepth parentAgent childAgent : Nat)
    (choice : Workspace.ChildChoice)
    (sourceWorkspace : DelegatedWorkspace := remoteWorkspace) : Case :=
  ⟨name, parentDepth, parentAgent, childAgent, sourceWorkspace, choice⟩

def observedBind : Workspace.ObservedWorkspace :=
  ⟨70, 2, some 71, .ready, true⟩

def cases : List Case :=
  [ mkCase "remote_depth_two_inherit_at_bound" 2 1 2
      (.inherit observedParentWorkspace)
  , mkCase "remote_depth_three_rejects_child" Subagent.maxSubagentDepth 1 2
      (.inherit observedParentWorkspace)
  , mkCase "readonly_parent_bind_readwrite_attenuates" 2 1 2
      (.bind observedBind (some .readWrite))
  , mkCase "readonly_parent_provision_distinct_identity" 2 1 2
      (.provision observedParentWorkspace true (some observedProvisionedWorkspace))
  , mkCase "provision_requires_verified_workspace" 2 1 2
      (.provision observedParentWorkspace true
        (some { observedProvisionedWorkspace with available := false }))
  , mkCase "provision_requires_exact_parent_path" 2 1 2
      (.provision observedParentWorkspace false (some observedProvisionedWorkspace))
  , mkCase "provision_rejects_changed_parent_seal" 2 1 2
      (.provision { observedParentWorkspace with sealHash := some 99 } true
        (some observedProvisionedWorkspace))
  , mkCase "provision_rejects_absent_to_present_parent_seal" 2 1 2
      (.provision observedParentWorkspace true (some observedProvisionedWorkspace))
      { remoteWorkspace with sealHash := none }
  , mkCase "provision_rejects_unavailable_parent" 2 1 2
      (.provision { observedParentWorkspace with available := false } true
        (some observedProvisionedWorkspace))
  , mkCase "provision_rejects_failed_creation" 2 1 2
      (.provision observedParentWorkspace true none)
  , mkCase "inherit_rejects_changed_seal" 2 1 2
      (.inherit { observedParentWorkspace with sealHash := some 99 })
  ]

def accepted (value : Case) : Option DelegatedCall :=
  acceptedDelegatedCallAtDepthWithWorkspace value.parentDepth value.sourceWorkspace

def expected (value : Case) : Option (Nat × Option DelegatedWorkspace) :=
  (accepted value).bind fun row =>
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
  | .provision observedParent parentPathExact createdChild =>
      "{\"kind\":\"provision\",\"observed_parent\":" ++ observedJson observedParent ++
      ",\"parent_path_exact\":" ++
      (if parentPathExact then "true" else "false") ++
      ",\"created_child\":" ++
      (createdChild.map observedJson).getD "null" ++ "}"

def caseJson (value : Case) : String :=
  let source := accepted value
  let result := expected value
  "{\"name\":" ++ jsonString value.name ++
    ",\"parent_depth\":" ++ toString value.parentDepth ++
    ",\"parent_agent\":" ++ toString value.parentAgent ++
    ",\"child_agent\":" ++ toString value.childAgent ++
    ",\"delegated_input\":" ++ (source.map (fun row =>
      "{\"source_close_doc_id\":" ++ toString row.input.source.closeId ++
      ",\"source_stream\":" ++ toString row.input.source.stream ++
      ",\"arguments\":" ++ jsonString row.input.arguments ++
      ",\"parent_subagent_depth\":" ++ toString row.input.parentSubagentDepth ++ "}")).getD "null" ++
    ",\"parent_workspace\":" ++ ((source.bind (·.workspace)).map stampJson).getD "null" ++
    ",\"choice\":" ++ choiceJson value.choice ++
    ",\"expected\":" ++ (result.map (fun (depth, workspace) =>
      "{\"child_depth\":" ++ toString depth ++ ",\"child_workspace\":" ++
      (workspace.map stampJson).getD "null" ++ "}")).getD "null" ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

theorem generated_cases_have_accepted_source :
    cases.all (fun value => (accepted value).isSome) = true := by
  native_decide

end Conformance.DelegatedChildContracts
