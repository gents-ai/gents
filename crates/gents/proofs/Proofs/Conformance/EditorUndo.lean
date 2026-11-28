import Proofs.Session.Workspace.EditorUndo

namespace Conformance.EditorUndo
open Workspace.EditorUndo

private def target : Target :=
  {reference := "node/session/request/tool/root/file", actions := ["physical-action"], uri := "file:///project/file",
    before := "before\n", after := "after\n", version := 4}
private def document : Document :=
  {uri := target.uri, content := target.after, version := target.version, writable := true}
private def observations : Observations := ⟨true, true, true, true, true⟩
private def other : Target :=
  {target with reference := "other-action", actions := ["other-physical-action"], uri := "file:///project/other", version := 7}
private def otherDocument : Document :=
  {document with uri := other.uri, version := other.version}

structure ApplyCase where
  name : String
  targets : List Target := [target]
  documents : List Document := [document]
  observations : Observations := observations
  accepted : Bool := false

def cases : List ApplyCase :=
  [ {name := "one_existing_text_document", accepted := true}
  , {name := "multiple_documents_initial_preflight", targets := [target, other],
      documents := [document, otherDocument], accepted := true}
  , {name := "unrelated_dirty_document_unchanged", documents :=
      [document, {otherDocument with content := "unrelated", dirty := true}], accepted := true}
  , {name := "workspace_untrusted", observations := {observations with trusted := false}}
  , {name := "selection_changed", observations := {observations with selectionCurrent := false}}
  , {name := "unverified_native_images", observations := {observations with sourcesVerified := false}}
  , {name := "mapping_changed", observations := {observations with mappingsCurrent := false}}
  , {name := "overwrite_policy_refuses_before_restoration",
      observations := {observations with preservesConflicts := false}}
  , {name := "empty_targets", targets := []}
  , {name := "duplicate_target", targets := [target, target]}
  , {name := "duplicate_observation", documents := [document, document]}
  , {name := "missing_document", documents := []}
  , {name := "missing_action_reference", targets := [{target with reference := ""}]}
  , {name := "missing_physical_actions", targets := [{target with actions := []}]}
  , {name := "empty_physical_action", targets := [{target with actions := [""]}]}
  , {name := "duplicate_physical_action", targets := [{target with actions := ["same", "same"]}]}
  , {name := "aliased_action_across_targets", targets := [target, {other with actions := target.actions}],
      documents := [document, otherDocument]}
  , {name := "missing_uri", targets := [{target with uri := ""}]}
  , {name := "missing_version", targets := [{target with version := 0}]}
  , {name := "empty_file_before_is_supported", targets := [{target with before := ""}], accepted := true}
  , {name := "no_recorded_change", targets := [{target with before := target.after}]}
  , {name := "unsupported_encoding", targets := [{target with encoding := "utf16le"}]}
  , {name := "mixed_eol", targets := [{target with eol := "mixed"}]}
  , {name := "postimage_changed", documents := [{document with content := "later edit"}]}
  , {name := "buffer_dirty", documents := [{document with dirty := true}]}
  , {name := "document_closed", documents := [{document with closed := true}]}
  , {name := "document_readonly", documents := [{document with writable := false}]}
  , {name := "document_encoding_changed", documents := [{document with encoding := "utf8bom"}]}
  , {name := "document_eol_changed", documents := [{document with eol := "crlf"}]}
  , {name := "version_changed_with_equal_text", documents := [{document with version := 6}]}
  , {name := "uri_changed", documents := [{document with uri := other.uri}]}
  , {name := "one_changed_document_refuses_all", targets := [target, other],
      documents := [document, {otherDocument with content := "later"}]}
  , {name := "one_dirty_document_refuses_all", targets := [target, other],
      documents := [document, {otherDocument with dirty := true}]}
  , {name := "binary_text", targets := [{target with before := "binary\u0000"}]}
  , {name := "bom_text", targets := [{target with before := "\uFEFFbefore"}]}
  , {name := "mixed_before_eol", targets := [{target with before := "a\r\nb\n"}]}
  , {name := "crlf_document", targets := [{target with eol := "crlf", before := "before\r\n", after := "after\r\n"}],
      documents := [{document with eol := "crlf", content := "after\r\n"}], accepted := true}
  ]

theorem expected_eligibility :
    cases.all (fun c => eligible c.targets c.documents c.observations == c.accepted) = true := by native_decide

private def safeSave : SaveObservations := ⟨true, true, true, true⟩

theorem missing_intermediate_image_is_not_composable :
    composeTextEdits [("a", "b"), ("missing", "c")] = none := by decide

theorem normal_save_participant_is_not_exact_restoration :
    finishSave ⟨.saving, false⟩ ⟨.saved, false⟩ true = ⟨.saved, false⟩ := by rfl

theorem external_edit_refuses_save :
    beginSave ⟨.pending, false⟩ {safeSave with diskPostimageCurrent := false} true =
      (⟨.conflict, false⟩, false) := by rfl

theorem configured_overwrite_refuses_save :
    beginSave ⟨.pending, false⟩ {safeSave with preservesConflicts := false} true =
      (⟨.failed, false⟩, false) := by rfl

private def sequence : Sequence :=
  ⟨[⟨target.reference, target.actions, target.uri, .fresh, ⟨.pending, false⟩⟩,
    ⟨other.reference, other.actions, other.uri, .fresh, ⟨.pending, false⟩⟩], false⟩
private def successful (index : Nat) : List Action :=
  [.startRestore index true true, .observeRestore index .applied true true,
   .startSave index safeSave true, .observeSave index ⟨.saved, true⟩ true]

theorem later_target_cannot_skip_first : (step sequence (.startRestore 1 true true)).2 = .none := by decide

theorem partial_conflict_keeps_first_saved :
    let s := (successful 0).foldl (fun s action => (step s action).1) sequence
    let failed := (step s (.startRestore 1 false true)).1
    failed.items[0]?.map (fun p => p.restore == .applied && p.save.state == .saved) = some true ∧
    failed.items[1]?.map (fun p => p.restore == .fresh) = some true ∧ failed.closed = true := by decide

private def completedSequence : Sequence :=
  (successful 0 ++ successful 1).foldl (fun s action => (step s action).1) sequence
private def oneSequence (t : Target) (restore : State) (closed : Bool) : Sequence :=
  ⟨[⟨t.reference, t.actions, t.uri, restore, ⟨.pending, false⟩⟩], closed⟩
private def chain : Target :=
  {target with reference := "complete-chain", actions := ["physical-action", "next-physical-action"]}
private def renamed : Target :=
  {target with reference := "changed-reference", uri := "file:///different-mapping"}
structure ReservationCase where
  name : String
  ledger : Ledger := []
  id : String := "new-explicit-operation"
  targets : List Target := [target]
  documents : List Document := [document]
  observations : Observations := observations
  persisted : Bool := true
  accepted : Bool := false
private def reservationCases : List ReservationCase :=
  [ {name := "fresh_reserves_every_physical_action", targets := [target, other],
      documents := [document, otherDocument], accepted := true}
  , {name := "missing_operation_identity", id := ""}
  , {name := "same_operation_identity_is_not_reused", id := "old", ledger := [⟨"old", completedSequence⟩]}
  , {name := "failed_reservation_write", persisted := false}
  , {name := "global_then_per_file_is_inert", ledger := [⟨"old", completedSequence⟩]}
  , {name := "per_file_then_global_refuses_all", ledger := [⟨"old", oneSequence target .applied true⟩],
      targets := [target, other], documents := [document, otherDocument]}
  , {name := "pending_global_blocks_per_file", ledger := [⟨"old", (step sequence (.startRestore 0 true true)).1⟩]}
  , {name := "unknown_cannot_be_regrouped", ledger := [⟨"old", oneSequence target .unknown true⟩]}
  , {name := "changed_reference_and_mapping_cannot_bypass", ledger := [⟨"old", completedSequence⟩],
      targets := [renamed], documents := [{document with uri := renamed.uri}]}
  , {name := "chain_then_subset_is_inert", ledger := [⟨"old", oneSequence chain .applied true⟩]}
  , {name := "subset_then_chain_is_inert", ledger := [⟨"old", oneSequence target .applied true⟩], targets := [chain]}
  , {name := "disjoint_actions_are_admitted", ledger := [⟨"old", oneSequence other .applied true⟩], accepted := true}
  , {name := "known_rejected_allows_new_explicit_attempt", ledger := [⟨"old", oneSequence target .rejected true⟩], accepted := true}
  , {name := "closed_untouched_allows_new_explicit_attempt", ledger := [⟨"old", oneSequence target .fresh true⟩], accepted := true}
  , {name := "open_untouched_is_reserved", ledger := [⟨"old", oneSequence target .fresh false⟩]}
  , {name := "partial_global_keeps_applied_reserved", ledger := [⟨"old", {completedSequence with
      items := [⟨target.reference, target.actions, target.uri, .applied, ⟨.failed, false⟩⟩, ⟨other.reference, other.actions, other.uri, .fresh, ⟨.pending, false⟩⟩] }⟩]}
  , {name := "partial_global_allows_only_untouched_target", ledger := [⟨"old", {completedSequence with
      items := [⟨target.reference, target.actions, target.uri, .applied, ⟨.saved, true⟩⟩, ⟨other.reference, other.actions, other.uri, .fresh, ⟨.pending, false⟩⟩] }⟩],
      targets := [other], documents := [otherDocument], accepted := true}
  , {name := "duplicate_prior_operation_fails_closed", ledger := [⟨"old", completedSequence⟩, ⟨"old", completedSequence⟩]}
  , {name := "corrupt_missing_physical_identity_fails_closed", ledger := [⟨"old", ⟨[⟨target.reference, [], target.uri, .applied, ⟨.saved, true⟩⟩], true⟩⟩]}
  , {name := "corrupt_no_effect_with_saved_marker_fails_closed", ledger := [⟨"old", ⟨[
      ⟨target.reference, target.actions, target.uri, .fresh, ⟨.saved, true⟩⟩], true⟩⟩]}
  ]

theorem expected_cross_group_reservations : reservationCases.all (fun c =>
    (reserve c.ledger c.id c.targets c.documents c.observations c.persisted).isSome == c.accepted) = true := by
  native_decide

end Conformance.EditorUndo
