import Proofs.Workspace.ChangeReview
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.ChangeReviewContracts
open Workspace.ChangeReview Conformance.Contracts
open Workspace.PathCapability (Disposition)

private def version (blob : String) : Version := ⟨blob, false⟩
private def reference : Reference :=
  ⟨⟨"workspace-1", "node-1", "base-tree", .exactPaths ["src/a.rs", "src/new.rs", "src/deleted.rs"],
      "after-tree"⟩, "request-document-1", "review-1"⟩
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

structure Case where
  name : String
  files : List File := files
  review : Review := review
  evidence : Evidence := evidence
  disposition : Disposition := .denied
  deriving Repr

def cases : List Case :=
  [ {name := "restore_edit_addition_and_deletion_preserving_user_index", disposition := .accepted}
  , {name := "unauthorized", evidence := {evidence with authorized := false}}
  , {name := "placement_unavailable", evidence := {evidence with placementVerified := false}}
  , {name := "wrong_node", evidence := {evidence with expected :=
      {reference with binding := {reference.binding with owner := "other-node"}}}}
  , {name := "wrong_workspace", evidence := {evidence with expected :=
      {reference with binding := {reference.binding with workspaceId := "other-workspace"}}}}
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
private def referenceJson (value : Reference) :=
  "{\"workspace_id\":" ++ jsonString value.binding.workspaceId ++
  ",\"owner\":" ++ jsonString value.binding.owner ++
  ",\"base\":" ++ jsonString value.binding.base ++
  ",\"capability\":" ++ capabilityJson value.binding.capability ++
  ",\"tree\":" ++ jsonString value.binding.tree ++
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
  ",\"before_tree\":" ++ jsonString c.review.beforeTree ++
  ",\"edits\":" ++ jsonArray (c.review.edits.map editJson) ++ "}" ++
  ",\"evidence\":{\"expected\":" ++ referenceJson c.evidence.expected ++
  ",\"authorized\":" ++ boolJson c.evidence.authorized ++
  ",\"placement_verified\":" ++ boolJson c.evidence.placementVerified ++
  ",\"snapshots_available\":" ++ boolJson c.evidence.snapshotsAvailable ++
  ",\"no_active_writer\":" ++ boolJson c.evidence.noActiveWriter ++
  ",\"receipt\":" ++ (c.evidence.receipt.map referenceJson).getD "null" ++ "}" ++
  ",\"expected_files\":" ++ jsonArray (outcome.1.map fileJson) ++
  ",\"disposition\":" ++ dispositionJson outcome.2 ++ "}"

def casesJson := jsonArray (cases.map caseJson)

end Conformance.ChangeReviewContracts
