import Proofs.Workspace.EditorUndo
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.EditorUndo
open Workspace.EditorUndo Conformance.Contracts

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

private def boolJson (value : Bool) := if value then "true" else "false"
private def targetJson (t : Target) :=
  "{\"reference\":" ++ jsonString t.reference ++ ",\"actions\":" ++ jsonArray (t.actions.map jsonString) ++ ",\"uri\":" ++ jsonString t.uri ++
  ",\"before\":" ++ jsonString t.before ++ ",\"after\":" ++ jsonString t.after ++
  ",\"version\":" ++ toString t.version ++ ",\"encoding\":" ++ jsonString t.encoding ++
  ",\"eol\":" ++ jsonString t.eol ++ "}"
private def documentJson (d : Document) :=
  "{\"uri\":" ++ jsonString d.uri ++ ",\"content\":" ++ jsonString d.content ++
  ",\"version\":" ++ toString d.version ++ ",\"encoding\":" ++ jsonString d.encoding ++
  ",\"eol\":" ++ jsonString d.eol ++ ",\"dirty\":" ++ boolJson d.dirty ++
  ",\"closed\":" ++ boolJson d.closed ++ ",\"writable\":" ++ boolJson d.writable ++ "}"
private def observationsJson (o : Observations) :=
  "{\"trusted\":" ++ boolJson o.trusted ++ ",\"selectionCurrent\":" ++ boolJson o.selectionCurrent ++
  ",\"sourcesVerified\":" ++ boolJson o.sourcesVerified ++
  ",\"mappingsCurrent\":" ++ boolJson o.mappingsCurrent ++
  ",\"preservesConflicts\":" ++ boolJson o.preservesConflicts ++ "}"
private def stateJson : State → String
  | .fresh => "\"fresh\"" | .pending => "\"pending\"" | .applied => "\"applied\""
  | .rejected => "\"rejected\"" | .unknown => "\"unknown\""
private def completionJson : Completion → String
  | .applied => "\"applied\"" | .rejected => "\"rejected\"" | .unknown => "\"unknown\""
private def applyJson (c : ApplyCase) :=
  "{\"kind\":\"apply\",\"name\":" ++ jsonString c.name ++
  ",\"targets\":" ++ jsonArray (c.targets.map targetJson) ++
  ",\"documents\":" ++ jsonArray (c.documents.map documentJson) ++
  ",\"observations\":" ++ observationsJson c.observations ++
  ",\"accepted\":" ++ boolJson (eligible c.targets c.documents c.observations) ++ "}"
private def states : List State := [.fresh, .pending, .applied, .rejected, .unknown]
private def bools := [false, true]
private def beginCases := states.flatMap fun state => bools.flatMap fun allowed => bools.map fun persisted =>
  let result := begin state allowed persisted
  "{\"kind\":\"begin\",\"state\":" ++ stateJson state ++
  ",\"allowed\":" ++ boolJson allowed ++ ",\"persisted\":" ++ boolJson persisted ++
  ",\"expected\":" ++ stateJson result.1 ++ ",\"effect\":" ++ boolJson result.2 ++ "}"
private def completionCases := states.flatMap fun state =>
  ([Completion.applied, .rejected, .unknown]).flatMap fun completion =>
  bools.flatMap fun observed => bools.map fun persisted =>
  "{\"kind\":\"complete\",\"state\":" ++ stateJson state ++
  ",\"completion\":" ++ completionJson completion ++
  ",\"observed\":" ++ boolJson observed ++ ",\"persisted\":" ++ boolJson persisted ++
  ",\"expected\":" ++ stateJson (complete state completion observed persisted) ++ "}"

private def saveStateJson : SaveState → String
  | .pending => "\"pending\"" | .saving => "\"saving\"" | .saved => "\"saved\""
  | .conflict => "\"conflict\"" | .failed => "\"failed\"" | .unknown => "\"unknown\""
private def saveJson (m : SaveMarker) :=
  "{\"state\":" ++ saveStateJson m.state ++ ",\"exact\":" ++ boolJson m.exact ++ "}"
private def saveObservationsJson (o : SaveObservations) :=
  "{\"preservesConflicts\":" ++ boolJson o.preservesConflicts ++
  ",\"documentCurrent\":" ++ boolJson o.documentCurrent ++
  ",\"diskPostimageCurrent\":" ++ boolJson o.diskPostimageCurrent ++
  ",\"bufferRecoverable\":" ++ boolJson o.bufferRecoverable ++ "}"
private def saveStates : List SaveState := [.pending, .saving, .saved, .conflict, .failed, .unknown]
private def safeSave : SaveObservations := ⟨true, true, true, true⟩
private def saveGuards : List SaveObservations :=
  [safeSave, {safeSave with preservesConflicts := false},
   {safeSave with documentCurrent := false}, {safeSave with diskPostimageCurrent := false},
   {safeSave with bufferRecoverable := false}]
private def saveBeginCases := saveStates.flatMap fun state =>
  saveGuards.flatMap fun observations => bools.map fun persisted =>
  let marker : SaveMarker := ⟨state, false⟩
  let result := beginSave marker observations persisted
  "{\"kind\":\"beginSave\",\"marker\":" ++ saveJson marker ++
  ",\"observations\":" ++ saveObservationsJson observations ++
  ",\"persisted\":" ++ boolJson persisted ++ ",\"expected\":" ++ saveJson result.1 ++
  ",\"effect\":" ++ boolJson result.2 ++ "}"
private def saveResults : List SaveMarker :=
  [⟨.saved, true⟩, ⟨.saved, false⟩, ⟨.conflict, false⟩, ⟨.failed, false⟩, ⟨.unknown, false⟩]
private def saveCompleteCases := saveStates.flatMap fun state =>
  saveResults.flatMap fun result => bools.map fun persisted =>
  let marker : SaveMarker := ⟨state, false⟩
  "{\"kind\":\"finishSave\",\"marker\":" ++ saveJson marker ++
  ",\"result\":" ++ saveJson result ++ ",\"persisted\":" ++ boolJson persisted ++
  ",\"expected\":" ++ saveJson (finishSave marker result persisted) ++ "}"
private def saveReopenCases := saveStates.map fun state =>
  let marker : SaveMarker := ⟨state, false⟩
  "{\"kind\":\"reopenSave\",\"marker\":" ++ saveJson marker ++
  ",\"expected\":" ++ saveJson (reopenSave marker) ++ "}"

private def composeCases : List (List (String × String)) :=
  [[], [("a", "b")], [("a", "b"), ("b", "c")],
   [("", "a"), ("a", "b"), ("b", "c")], [("a", "b"), ("other", "c")],
   [("a", "a")], [("a", "b"), ("b", "b")], [("a", "b"), ("b", "a")]]
private def pairJson (pair : String × String) := jsonArray [jsonString pair.1, jsonString pair.2]
private def composeJson := composeCases.map fun edits =>
  "{\"kind\":\"compose\",\"edits\":" ++ jsonArray (edits.map pairJson) ++
  ",\"expected\":" ++ ((composeTextEdits edits).map pairJson).getD "null" ++ "}"

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
private def traces : List (String × List Action) :=
  [ ("ordered_two_file_restore_and_save", successful 0 ++ successful 1)
  , ("later_target_changed", successful 0 ++ [.startRestore 1 false true] ++ successful 1)
  , ("editor_lock_refused", [.startRestore 0 true true,
      .observeRestore 0 .rejected false true] ++ successful 1)
  , ("restore_transport_unknown", [.startRestore 0 true true,
      .observeRestore 0 .unknown false true] ++ successful 1)
  , ("restore_result_marker_failed", [.startRestore 0 true true,
      .observeRestore 0 .applied true false] ++ successful 1)
  , ("restore_start_marker_failed", [.startRestore 0 true false] ++ successful 1)
  , ("save_start_marker_failed", (successful 0).take 2 ++ [.startSave 0 safeSave false] ++ successful 1)
  , ("save_receipt_marker_failed", (successful 0).take 3 ++
      [.observeSave 0 ⟨.saved, true⟩ false] ++ successful 1)
  , ("save_conflict_preserves_remaining_targets", (successful 0).take 2 ++
      [.startSave 0 {safeSave with diskPostimageCurrent := false} true] ++ successful 1)
  , ("save_failed_preserves_remaining_targets", (successful 0).take 3 ++
      [.observeSave 0 ⟨.failed, false⟩ true] ++ successful 1)
  , ("save_participant_change_is_reported", (successful 0).take 3 ++
      [.observeSave 0 ⟨.saved, false⟩ true] ++ successful 1)
  , ("cannot_skip_unsaved_first_target", [.startRestore 1 true true] ++ successful 0 ++ successful 1)
  , ("completed_replay_is_inert", successful 0 ++ successful 1 ++ successful 0)
  , ("normal_editor_auto_save_observed", (successful 0).take 2 ++
      [.observeSavedByEditor 0 ⟨.saved, true⟩ true] ++ successful 1)
  , ("normal_editor_save_receipt_write_failed", (successful 0).take 2 ++
      [.observeSavedByEditor 0 ⟨.saved, true⟩ false] ++ successful 1)
  ]
private def sequenceJson (s : Sequence) :=
  "{\"items\":" ++ jsonArray (s.items.map fun p =>
    "{\"reference\":" ++ jsonString p.reference ++ ",\"actions\":" ++ jsonArray (p.actions.map jsonString) ++ ",\"uri\":" ++ jsonString p.uri ++
    ",\"restore\":" ++ stateJson p.restore ++ ",\"save\":" ++ saveJson p.save ++ "}") ++
  ",\"closed\":" ++ boolJson s.closed ++ "}"
private def admissionCases := cases.flatMap fun c => bools.map fun persisted =>
  "{\"kind\":\"admitSequence\",\"name\":" ++ jsonString c.name ++
  ",\"targets\":" ++ jsonArray (c.targets.map targetJson) ++
  ",\"documents\":" ++ jsonArray (c.documents.map documentJson) ++
  ",\"observations\":" ++ observationsJson c.observations ++
  ",\"persisted\":" ++ boolJson persisted ++ ",\"expected\":" ++
  ((admitSequence c.targets c.documents c.observations persisted).map sequenceJson).getD "null" ++ "}"
private def effectJson : Effect → String
  | .none => "\"none\"" | .restore => "\"restore\"" | .save => "\"save\""
private def actionJson : Action → String
  | .startRestore index allowed persisted =>
    "{\"kind\":\"startRestore\",\"index\":" ++ toString index ++
    ",\"allowed\":" ++ boolJson allowed ++ ",\"persisted\":" ++ boolJson persisted ++ "}"
  | .observeRestore index completion observed persisted =>
    "{\"kind\":\"observeRestore\",\"index\":" ++ toString index ++
    ",\"completion\":" ++ completionJson completion ++ ",\"observed\":" ++ boolJson observed ++
    ",\"persisted\":" ++ boolJson persisted ++ "}"
  | .startSave index observations persisted =>
    "{\"kind\":\"startSave\",\"index\":" ++ toString index ++
    ",\"observations\":" ++ saveObservationsJson observations ++
    ",\"persisted\":" ++ boolJson persisted ++ "}"
  | .observeSave index result persisted =>
    "{\"kind\":\"observeSave\",\"index\":" ++ toString index ++
    ",\"result\":" ++ saveJson result ++ ",\"persisted\":" ++ boolJson persisted ++ "}"
  | .observeSavedByEditor index result persisted =>
    "{\"kind\":\"observeSavedByEditor\",\"index\":" ++ toString index ++
    ",\"result\":" ++ saveJson result ++ ",\"persisted\":" ++ boolJson persisted ++ "}"
private def traceJson (name : String) (s : Sequence) : List Action → List String
  | [] => ["{\"kind\":\"reopenSequence\",\"name\":" ++ jsonString name ++
      ",\"sequence\":" ++ sequenceJson s ++ ",\"expected\":" ++ sequenceJson (reopen s) ++ "}"]
  | action :: rest =>
    let result := step s action
    ("{\"kind\":\"step\",\"name\":" ++ jsonString name ++
      ",\"sequence\":" ++ sequenceJson s ++ ",\"action\":" ++ actionJson action ++
      ",\"expected\":" ++ sequenceJson result.1 ++ ",\"effect\":" ++ effectJson result.2 ++ "}") ::
      traceJson name result.1 rest

theorem later_target_cannot_skip_first : (step sequence (.startRestore 1 true true)).2 = .none := by decide

theorem partial_conflict_keeps_first_saved :
    let s := (successful 0).foldl (fun s action => (step s action).1) sequence
    let failed := (step s (.startRestore 1 false true)).1
    failed.items[0]?.map (fun p => p.restore == .applied && p.save.state == .saved) = some true ∧
    failed.items[1]?.map (fun p => p.restore == .fresh) = some true ∧ failed.closed = true := by decide


private def ledgerJson (ledger : Ledger) :=
  jsonArray (ledger.map fun operation =>
    "{\"id\":" ++ jsonString operation.id ++ ",\"sequence\":" ++ sequenceJson operation.sequence ++ "}")
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

private def reservationJson := reservationCases.map fun c =>
  "{\"kind\":\"reserve\",\"name\":" ++ jsonString c.name ++
  ",\"ledger\":" ++ ledgerJson c.ledger ++ ",\"id\":" ++ jsonString c.id ++
  ",\"targets\":" ++ jsonArray (c.targets.map targetJson) ++
  ",\"documents\":" ++ jsonArray (c.documents.map documentJson) ++
  ",\"observations\":" ++ observationsJson c.observations ++
  ",\"persisted\":" ++ boolJson c.persisted ++ ",\"expected\":" ++
  ((reserve c.ledger c.id c.targets c.documents c.observations c.persisted).map ledgerJson).getD "null" ++ "}"

def casesJson := jsonArray (cases.map applyJson ++ beginCases ++ completionCases ++
  saveBeginCases ++ saveCompleteCases ++ saveReopenCases ++ composeJson ++ admissionCases ++
  (traces.flatMap fun (name, actions) => traceJson name sequence actions) ++ reservationJson)
end Conformance.EditorUndo

def main : IO Unit := IO.println Conformance.EditorUndo.casesJson
