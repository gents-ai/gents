import Proofs.Workspace.ChangeReview
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.ChangeReviewContracts
open Workspace.ChangeReview Conformance.Contracts
open Workspace.PathCapability (Disposition)

private def version (blob : String) : Version := ⟨blob, false⟩
private def workspaceBinding : Workspace.PathCapability.Binding :=
  ⟨"workspace-1", "node-1", "base-tree", .exactPaths ["src/a.rs", "src/new.rs", "src/deleted.rs"],
      "after-tree"⟩
private def reference : Reference :=
  ⟨.workspace workspaceBinding, "request-document-1", "review-1"⟩
private def requestReference : Reference :=
  ⟨.requestResource "node-1" "/projects/ordinary" workspaceBinding.capability "after-snapshot",
    "request-document-1", "review-1"⟩
private def review : Review :=
  ⟨reference, "before-tree",
    [⟨"src/a.rs", some (version "old"), some (version "new"), true⟩,
     ⟨"src/new.rs", none, some (version "created"), true⟩,
     ⟨"src/deleted.rs", some (version "deleted"), none, true⟩]⟩
private def files : List File :=
  [⟨"src/a.rs", some (version "new"), some (version "user-staged"), false, true⟩,
   ⟨"src/new.rs", some (version "created"), none, false, true⟩,
   ⟨"src/deleted.rs", none, some (version "user-staged-deletion"), false, true⟩,
   ⟨"unrelated.rs", some (version "unrelated"), some (version "staged-unrelated"), true, true⟩]
private def evidence : Evidence := ⟨reference, true, true, true, true, none⟩
private def requestReview : Review := {review with reference := requestReference}
private def requestEvidence : Evidence := {evidence with expected := requestReference}

structure Case where
  name : String
  files : List File := files
  review : Review := review
  evidence : Evidence := evidence
  disposition : Disposition := .denied
  deriving Repr

private def resourceCase (name node root : String) (capability : WorkspacePathCapability)
    (snapshot : String) : Case :=
  let reference := {requestReference with scope := .requestResource node root capability snapshot}
  {name, review := {requestReview with reference},
    evidence := {requestEvidence with expected := reference}}

def cases : List Case :=
  [ {name := "restore_edit_addition_and_deletion_preserving_user_index", disposition := .accepted}
  , {name := "unauthorized", evidence := {evidence with authorized := false}}
  , {name := "resource_unavailable", evidence := {evidence with resourceVerified := false}}
  , {name := "wrong_node", evidence := {evidence with expected :=
      {reference with scope := .workspace {workspaceBinding with owner := "other-node"}}}}
  , {name := "wrong_workspace", evidence := {evidence with expected :=
      {reference with scope := .workspace {workspaceBinding with workspaceId := "other-workspace"}}}}
  , {name := "wrong_request", evidence := {evidence with expected :=
      {reference with requestDocId := "other-request"}}}
  , {name := "stale_review", evidence := {evidence with expected :=
      {reference with reviewId := "previous-review"}}}
  , {name := "changed_file_refuses_whole_restore", files := files.map fun file =>
      if file.path == "src/a.rs" then {file with content := some (version "user-followup")} else file}
  , {name := "changed_file_mode_refuses_whole_restore", files := files.map fun file =>
      if file.path == "src/a.rs" then {file with content := some ⟨"new", true⟩} else file}
  , {name := "unsaved_target_refuses_whole_restore", files := files.map fun file =>
      if file.path == "src/a.rs" then {file with unsaved := true} else file}
  , {name := "unsafe_entry_refuses_whole_restore", files := files.map fun file =>
      if file.path == "src/a.rs" then {file with safeEntry := false} else file}
  , {name := "missing_observation_is_not_a_missing_file", files := files.filter fun file =>
      file.path != "src/deleted.rs"}
  , {name := "ambiguous_file_observation", files := files ++ files.take 1}
  , {name := "duplicate_recorded_path", review := {review with edits := review.edits ++ review.edits.take 1}}
  , {name := "noncanonical_recorded_path",
      review := {review with edits := review.edits.map (fun edit => {edit with canonical := false})}}
  , {name := "unowned_recorded_path", review := {review with edits :=
      review.edits ++ [⟨"outside.rs", none, some (version "outside"), true⟩]}}
  , {name := "empty_recorded_delta", review := {review with edits := []}}
  , {name := "no_effect_is_not_an_edit", review := {review with edits :=
      [⟨"src/a.rs", some (version "new"), some (version "new"), true⟩]}}
  , {name := "missing_blob_is_not_an_empty_file", review := {review with edits :=
      [⟨"src/a.rs", some (version ""), some (version "new"), true⟩]}}
  , {name := "missing_snapshots", evidence := {evidence with snapshotsAvailable := false}}
  , {name := "active_writer", evidence := {evidence with noActiveWriter := false}}
  , {name := "foreign_receipt",
      evidence := {evidence with receipt := some {reference with reviewId := "other-review"}}}
  , {name := "exact_receipt_replay_preserves_later_user_edits",
      files := files.map (fun file => {file with content := some (version "later-user-edit"), unsaved := true}),
      evidence := {evidence with receipt := some reference, snapshotsAvailable := false, noActiveWriter := false},
      disposition := .recovered}
  , {name := "ordinary_request_needs_no_managed_workspace", review := requestReview,
      evidence := requestEvidence, disposition := .accepted}
  , {name := "managed_scope_cannot_replace_request_resource", review := requestReview}
  , {name := "wrong_request_resource_root", review := requestReview,
      evidence := {requestEvidence with expected := {requestReference with scope := .requestResource "node-1" "/projects/other" workspaceBinding.capability "after-snapshot"}}}
  , {name := "wrong_request_resource_node", review := requestReview,
      evidence := {requestEvidence with expected := {requestReference with scope := .requestResource "other-node" "/projects/ordinary" workspaceBinding.capability "after-snapshot"}}}
  , resourceCase "request_resource_missing_node" "" "/projects/ordinary"
      workspaceBinding.capability "after-snapshot"
  , resourceCase "request_resource_missing_root" "node-1" ""
      workspaceBinding.capability "after-snapshot"
  , resourceCase "request_resource_missing_snapshot" "node-1" "/projects/ordinary"
      workspaceBinding.capability ""
  , resourceCase "request_resource_unrestricted_is_not_an_undo_grant" "node-1" "/projects/ordinary"
      .unrestrictedCompatibility "after-snapshot"
  , {name := "request_resource_not_verified", review := requestReview,
      evidence := {requestEvidence with resourceVerified := false}}
  , {name := "request_resource_receipt_replay_preserves_later_edits", review := requestReview,
      files := files.map (fun file => {file with content := some (version "later-user-edit"), unsaved := true}),
      evidence := {requestEvidence with receipt := some requestReference, snapshotsAvailable := false, noActiveWriter := false}, disposition := .recovered}
  ]

theorem regression_dispositions :
    cases.all (fun c => (execute c.files c.review c.evidence).2 == c.disposition) = true := by decide

theorem accepted_restores_originals :
    ((execute files review evidence).1.map File.content) =
      [some (version "old"), none, some (version "deleted"), some (version "unrelated")] := by decide

private def boolJson (value : Bool) := if value then "true" else "false"
private def versionJson : Option Version → String
  | none => "null"
  | some value => "{\"blob\":" ++ jsonString value.blob ++
      ",\"executable\":" ++ boolJson value.executable ++ "}"
private def capabilityJson : WorkspacePathCapability → String
  | .unrestrictedCompatibility => "{\"mode\":\"unrestrictedCompatibility\"}"
  | .exactPaths paths => "{\"mode\":\"exactPaths\",\"paths\":" ++ jsonStringArray paths ++ "}"
private def scopeJson : Scope → String
  | .workspace binding =>
      "{\"kind\":\"workspace\",\"workspace_id\":" ++ jsonString binding.workspaceId ++
      ",\"owner\":" ++ jsonString binding.owner ++
      ",\"base\":" ++ jsonString binding.base ++
      ",\"capability\":" ++ capabilityJson binding.capability ++
      ",\"tree\":" ++ jsonString binding.tree ++ "}"
  | .requestResource nodeDid root capability snapshot =>
      "{\"kind\":\"requestResource\",\"node_did\":" ++ jsonString nodeDid ++
      ",\"root\":" ++ jsonString root ++ ",\"capability\":" ++ capabilityJson capability ++
      ",\"snapshot\":" ++ jsonString snapshot ++ "}"
private def referenceJson (value : Reference) :=
  "{\"scope\":" ++ scopeJson value.scope ++
  ",\"request_doc_id\":" ++ jsonString value.requestDocId ++
  ",\"review_id\":" ++ jsonString value.reviewId ++ "}"
private def fileJson (value : File) :=
  "{\"path\":" ++ jsonString value.path ++
  ",\"content\":" ++ versionJson value.content ++
  ",\"staged\":" ++ versionJson value.staged ++
  ",\"unsaved\":" ++ boolJson value.unsaved ++
  ",\"safe_entry\":" ++ boolJson value.safeEntry ++ "}"
private def editJson (value : Edit) :=
  "{\"path\":" ++ jsonString value.path ++
  ",\"before\":" ++ versionJson value.before ++
  ",\"after\":" ++ versionJson value.after ++
  ",\"canonical\":" ++ boolJson value.canonical ++ "}"
private def dispositionJson : Disposition → String
  | .accepted => "\"accepted\"" | .recovered => "\"recovered\"" | .denied => "\"denied\""
private def caseJson (c : Case) :=
  let outcome := execute c.files c.review c.evidence
  "{\"name\":" ++ jsonString c.name ++
  ",\"files\":" ++ jsonArray (c.files.map fileJson) ++
  ",\"review\":{\"reference\":" ++ referenceJson c.review.reference ++
  ",\"before_snapshot\":" ++ jsonString c.review.beforeSnapshot ++
  ",\"edits\":" ++ jsonArray (c.review.edits.map editJson) ++ "}" ++
  ",\"evidence\":{\"expected\":" ++ referenceJson c.evidence.expected ++
  ",\"authorized\":" ++ boolJson c.evidence.authorized ++
  ",\"resource_verified\":" ++ boolJson c.evidence.resourceVerified ++
  ",\"snapshots_available\":" ++ boolJson c.evidence.snapshotsAvailable ++
  ",\"no_active_writer\":" ++ boolJson c.evidence.noActiveWriter ++
  ",\"receipt\":" ++ (c.evidence.receipt.map referenceJson).getD "null" ++ "}" ++
  ",\"expected_files\":" ++ jsonArray (outcome.1.map fileJson) ++
  ",\"disposition\":" ++ dispositionJson outcome.2 ++ "}"

def casesJson := jsonArray (cases.map caseJson)

end Conformance.ChangeReviewContracts
