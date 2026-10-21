import Proofs.Workspace.EditorUndoCheckpoint
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.EditorUndoCheckpoint
open Workspace.ChangeReview.Editor Workspace.ChangeReview.Editor.Checkpoint Conformance.Contracts

private def target : Target :=
  {reference := "node/session/request/tool/root/file", actions := ["physical-action"],
   uri := "file:///project/file", before := "before\n", after := "after\n", version := 4}
private def document : Document :=
  {uri := target.uri, content := target.after, version := target.version, writable := true}
private def observations : Observations := ⟨true, true, true, true, true⟩
private def saveObservations : SaveObservations := ⟨true, true, true, true⟩
private def durable : Receipt := ⟨true, true, true⟩
private def admit (caller : Nat := 1) (id : String := "operation") : Input :=
  .admit caller id [target] [document] observations
private def reserveInputs : List Input :=
  [.acquire 1 .acquired, .load 1 .empty, admit, .finishWrite 1 durable]
private def startRestore : List Input :=
  [.advance 1 (.startRestore 0 true true), .finishWrite 1 durable, .launch 1]
private def restored : List Input :=
  startRestore ++ [.settle 1, .advance 1 (.observeRestore 0 .applied true true), .finishWrite 1 durable]
private def startSave : List Input :=
  [.advance 1 (.startSave 0 saveObservations true), .finishWrite 1 durable, .launch 1]
private def saved : List Input :=
  startSave ++ [.settle 1, .advance 1 (.observeSave 0 ⟨.saved, true⟩ true), .finishWrite 1 durable]
private def reservedSequence : Sequence := (admitSequence [target] [document] observations true).getD ⟨[], true⟩
private def pendingSequence : Sequence := (Workspace.ChangeReview.Editor.step reservedSequence (.startRestore 0 true true)).1
private def restoredSequence : Sequence :=
  (Workspace.ChangeReview.Editor.step pendingSequence (.observeRestore 0 .applied true true)).1
private def savingSequence : Sequence := (Workspace.ChangeReview.Editor.step restoredSequence (.startSave 0 saveObservations true)).1
private def completedSequence : Sequence :=
  (Workspace.ChangeReview.Editor.step savingSequence (.observeSave 0 ⟨.saved, true⟩ true)).1
private def retained (sequence : Sequence) : World := ⟨.present 1 [⟨"old", sequence⟩], none, []⟩
private def replay : List Input := [.acquire 2 .acquired, .load 2 .empty, admit 2 "new", .launch 2, .release 2]

structure Case where
  name : String
  initial : World := {}
  inputs : List Input
  expected : List Output

private def accepted : List Output := [.acquired, .loaded, .checkpoint, .acknowledged]
private def restoreOutputs : List Output :=
  [.checkpoint, .acknowledged, .effect .restore, .settled, .checkpoint, .acknowledged]
private def saveOutputs : List Output :=
  [.checkpoint, .acknowledged, .effect .save, .settled, .checkpoint, .acknowledged]
private def noReplay : List Output := [.acquired, .loaded, .refused, .refused, .released]
private def unresolvedReplay : List Output := [.acquired, .refused, .refused, .refused, .released]
private def differentTarget (sameUri : Bool) : Target :=
  let uri := if sameUri then target.uri else "file:///project/other"
  {target with reference := "different-retained-reference", actions := ["different-physical-action"], uri}
private def differentAdmission (sameUri : Bool) : Input :=
  let next := differentTarget sameUri
  .admit 2 "different-operation" [next] [{document with uri := next.uri}] observations
private def deadRestore : List Input := reserveInputs ++ startRestore ++ [.helperLost 1 false]
private def deadSave : List Input := reserveInputs ++ restored ++ startSave ++ [.helperLost 1 false]
private def afterDeath (sameUri : Bool) : List Input :=
  [.acquire 2 .acquired, .load 2 .empty, differentAdmission sameUri,
   .finishWrite 1 durable, .launch 1, .settle 1, .settleOrphan 1,
   differentAdmission sameUri, .release 2]
private def afterDeathOutputs : List Output :=
  [.acquired, .refused, .refused, .refused, .refused, .refused, .orphanSettled, .refused, .released]
private def otherTarget : Target := {differentTarget false with actions := ["other-action"]}
private def twoAdmission : Input := .admit 1 "operation" [target, otherTarget]
  [document, {document with uri := otherTarget.uri}] observations

def cases : List Case :=
  [ {name := "restore_and_normal_save_require_separate_durable_acknowledgements",
      inputs := reserveInputs ++ restored ++ saved ++ [.release 1],
      expected := accepted ++ restoreOutputs ++ saveOutputs ++ [.released]}
  , {name := "second_window_cannot_acquire_held_guard", inputs :=
      [.acquire 1 .acquired, .acquire 2 .acquired, .load 2 .empty, .release 2, .load 1 .empty],
      expected := [.acquired, .busy, .refused, .refused, .loaded]}
  , {name := "real_os_lock_busy", inputs := [.acquire 1 .busy, .load 1 .empty, .launch 1],
      expected := [.busy, .refused, .refused]}
  , {name := "unavailable_guard_refuses", inputs := [.acquire 1 .unavailable, .load 1 .empty],
      expected := [.refused, .refused]}
  , {name := "corrupt_or_truncated_journal_refuses", initial := ⟨.corrupt, none, []⟩,
      inputs := [.acquire 1 .acquired, .load 1 .empty, admit, .launch 1],
      expected := [.acquired, .refused, .refused, .refused]}
  , {name := "unreadable_journal_is_not_empty", initial := ⟨.unavailable, none, []⟩,
      inputs := [.acquire 1 .acquired, .load 1 .empty, admit], expected := [.acquired, .refused, .refused]}
  , {name := "unsupported_envelope_version_refuses", initial := ⟨.present 2 [], none, []⟩,
      inputs := [.acquire 1 .acquired, .load 1 .empty, admit], expected := [.acquired, .refused, .refused]}
  , {name := "malformed_ref_only_ledger_refuses", initial := ⟨.present 1 [⟨"", reservedSequence⟩], none, []⟩,
      inputs := [.acquire 1 .acquired, .load 1 .empty, admit], expected := [.acquired, .refused, .refused]}
  , {name := "reservation_write_failure_poisoned_until_new_explicit_session", inputs :=
      [.acquire 1 .acquired, .load 1 .empty, admit, .finishWrite 1 ⟨false, false, false⟩,
       .finishWrite 1 durable, .launch 1, .release 1, .acquire 2 .acquired, .load 2 .empty, admit 2 "new"],
      expected := [.acquired, .loaded, .checkpoint, .refused, .refused, .refused,
       .released, .acquired, .loaded, .checkpoint]}
  , {name := "installed_reservation_lost_ack_is_inert_and_reserved", inputs :=
      [.acquire 1 .acquired, .load 1 .empty, admit, .finishWrite 1 ⟨true, true, false⟩,
       .launch 1, .release 1] ++ replay,
      expected := [.acquired, .loaded, .checkpoint, .refused, .refused, .released] ++ unresolvedReplay}
  , {name := "directory_sync_unknown_never_grants_effect", inputs :=
      [.acquire 1 .acquired, .load 1 .empty, admit, .finishWrite 1 ⟨true, false, false⟩,
       .launch 1, .release 1] ++ replay,
      expected := [.acquired, .loaded, .checkpoint, .refused, .refused, .released] ++ unresolvedReplay}
  , {name := "ack_without_durability_is_not_success", inputs :=
      [.acquire 1 .acquired, .load 1 .empty, admit, .finishWrite 1 ⟨true, false, true⟩, .launch 1],
      expected := [.acquired, .loaded, .checkpoint, .refused, .refused]}
  , {name := "durability_without_installation_is_not_success", inputs :=
      [.acquire 1 .acquired, .load 1 .empty, admit, .finishWrite 1 ⟨false, true, true⟩, .launch 1],
      expected := [.acquired, .loaded, .checkpoint, .refused, .refused]}
  , {name := "pending_restoration_replay_is_inert", initial := retained pendingSequence,
      inputs := replay, expected := unresolvedReplay}
  , {name := "pending_save_replay_is_inert", initial := retained savingSequence,
      inputs := replay, expected := unresolvedReplay}
  , {name := "completed_operation_replay_is_inert", initial := retained completedSequence,
      inputs := replay, expected := noReplay}
  , {name := "known_rejected_allows_only_a_new_explicit_attempt", initial := retained
      ⟨[{reference := target.reference, actions := target.actions, uri := target.uri,
          restore := .rejected, save := ⟨.pending, false⟩}], true⟩,
      inputs := [.acquire 2 .acquired, .load 2 .empty, admit 2 "new"],
      expected := [.acquired, .loaded, .checkpoint]}
  , {name := "cancel_during_checkpoint_cannot_release_or_launch", inputs :=
      [.acquire 1 .acquired, .load 1 .empty, admit, .cancel 1, .release 1,
       .finishWrite 1 durable, .launch 1, .release 1] ++ replay,
      expected := [.acquired, .loaded, .checkpoint, .cancelled, .held, .acknowledged,
       .refused, .released] ++ unresolvedReplay}
  , {name := "cancelled_restore_keeps_guard_until_actual_settlement", inputs :=
      reserveInputs ++ startRestore ++ [.cancel 1, .release 1, .acquire 2 .acquired,
       .advance 1 (.observeRestore 0 .applied true true), .settle 1,
       .advance 1 (.observeRestore 0 .applied true true), .finishWrite 1 durable,
       .advance 1 (.startSave 0 saveObservations true), .release 1],
      expected := accepted ++ [.checkpoint, .acknowledged, .effect .restore, .cancelled,
       .held, .busy, .refused, .settled, .checkpoint, .acknowledged, .refused, .released]}
  , {name := "cancelled_save_keeps_guard_until_actual_settlement", inputs :=
      reserveInputs ++ restored ++ startSave ++ [.cancel 1, .release 1, .settle 1,
       .advance 1 (.observeSave 0 ⟨.saved, true⟩ true), .finishWrite 1 durable, .release 1],
      expected := accepted ++ restoreOutputs ++ [.checkpoint, .acknowledged, .effect .save,
       .cancelled, .held, .settled, .checkpoint, .acknowledged, .released]}
  , {name := "restore_marker_installed_but_lost_ack_never_launches", inputs := reserveInputs ++
      [.advance 1 (.startRestore 0 true true), .finishWrite 1 ⟨true, true, false⟩,
       .launch 1, .release 1] ++ replay,
      expected := accepted ++ [.checkpoint, .refused, .refused, .released] ++ unresolvedReplay}
  , {name := "checkpoint_in_flight_holds_guard", inputs :=
      [.acquire 1 .acquired, .load 1 .empty, admit, .release 1, .acquire 2 .acquired],
      expected := [.acquired, .loaded, .checkpoint, .held, .busy]}
  , {name := "stale_connection_cannot_use_released_effect", inputs := reserveInputs ++
      [.advance 1 (.startRestore 0 true true), .finishWrite 1 durable, .release 1,
       .acquire 2 .acquired, .launch 1, .settle 1, .load 2 .empty, admit 2 "new"],
      expected := accepted ++ [.checkpoint, .acknowledged, .released, .acquired,
       .refused, .refused, .refused, .refused]}
  , {name := "foreign_write_cannot_change_owned_pending_candidate", inputs :=
      [.acquire 1 .acquired, .load 1 .empty, admit, .finishWrite 2 durable, .launch 2,
       .finishWrite 1 durable],
      expected := [.acquired, .loaded, .checkpoint, .refused, .refused, .acknowledged]}
  , {name := "helper_death_during_restore_blocks_different_action_same_uri",
      inputs := deadRestore ++ afterDeath true,
      expected := accepted ++ [.checkpoint, .acknowledged, .effect .restore, .helperLost] ++ afterDeathOutputs}
  , {name := "helper_death_during_restore_blocks_different_action_different_uri",
      inputs := deadRestore ++ afterDeath false,
      expected := accepted ++ [.checkpoint, .acknowledged, .effect .restore, .helperLost] ++ afterDeathOutputs}
  , {name := "helper_death_during_save_blocks_different_action_same_uri",
      inputs := deadSave ++ afterDeath true,
      expected := accepted ++ restoreOutputs ++ [.checkpoint, .acknowledged, .effect .save, .helperLost] ++ afterDeathOutputs}
  , {name := "helper_death_during_save_blocks_different_action_different_uri",
      inputs := deadSave ++ afterDeath false,
      expected := accepted ++ restoreOutputs ++ [.checkpoint, .acknowledged, .effect .save, .helperLost] ++ afterDeathOutputs}
  , {name := "late_orphan_settlement_does_not_clear_barrier_for_third_window",
      inputs := deadRestore ++ [.settleOrphan 1, .acquire 3 .acquired, .load 3 .empty,
        .admit 3 "third-operation" [differentTarget false] [{document with uri := "file:///project/other"}] observations],
      expected := accepted ++ [.checkpoint, .acknowledged, .effect .restore, .helperLost,
        .orphanSettled, .acquired, .refused, .refused]}
  , {name := "cancel_then_helper_death_preserves_same_workspace_barrier",
      inputs := reserveInputs ++ startRestore ++ [.cancel 1, .release 1, .helperLost 1 false] ++ afterDeath true,
      expected := accepted ++ [.checkpoint, .acknowledged, .effect .restore,
        .cancelled, .held, .helperLost] ++ afterDeathOutputs}
  , {name := "helper_death_during_unacknowledged_admission_before_install_has_no_effect",
      inputs := [.acquire 1 .acquired, .load 1 .empty, admit, .helperLost 1 false,
        .finishWrite 1 durable, .launch 1, .acquire 2 .acquired, .load 2 .empty, admit 2 "new"],
      expected := [.acquired, .loaded, .checkpoint, .helperLost, .refused, .refused,
        .acquired, .loaded, .checkpoint]}
  , {name := "helper_death_during_unacknowledged_admission_after_install_is_inert",
      inputs := [.acquire 1 .acquired, .load 1 .empty, admit, .helperLost 1 true] ++ replay,
      expected := [.acquired, .loaded, .checkpoint, .helperLost] ++ unresolvedReplay}
  , {name := "closed_untouched_allows_new_explicit_attempt", initial := retained {reservedSequence with closed := true},
      inputs := [.acquire 2 .acquired, .load 2 .empty, admit 2 "new"],
      expected := [.acquired, .loaded, .checkpoint]}
  , {name := "closed_unknown_blocks_distinct_action_same_uri", initial := retained
      {reservedSequence with items := reservedSequence.items.map fun progress => {progress with restore := .unknown}, closed := true},
      inputs := [.acquire 2 .acquired, .load 2 .empty, differentAdmission true],
      expected := [.acquired, .refused, .refused]}
  , {name := "closed_unknown_save_blocks_distinct_action_different_uri", initial := retained
      {restoredSequence with items := restoredSequence.items.map fun progress => {progress with save := ⟨.unknown, false⟩}, closed := true},
      inputs := [.acquire 2 .acquired, .load 2 .empty, differentAdmission false],
      expected := [.acquired, .refused, .refused]}
  , {name := "known_save_conflict_permits_disjoint_explicit_operation", initial := retained
      (Workspace.ChangeReview.Editor.step restoredSequence (.startSave 0 {saveObservations with diskPostimageCurrent := false} true)).1,
      inputs := [.acquire 2 .acquired, .load 2 .empty, differentAdmission false],
      expected := [.acquired, .loaded, .checkpoint]}
  , {name := "absent_journal_with_known_old_memento_records_refuses",
      inputs := [.acquire 1 .acquired, .load 1 .records, admit, .launch 1],
      expected := [.acquired, .refused, .refused, .refused]}
  , {name := "absent_journal_with_unavailable_old_memento_observation_refuses",
      inputs := [.acquire 1 .acquired, .load 1 .unavailable, admit, .launch 1],
      expected := [.acquired, .refused, .refused, .refused]}
  , {name := "present_journal_does_not_silently_discard_old_memento_records",
      initial := retained completedSequence,
      inputs := [.acquire 1 .acquired, .load 1 .records, admit, .launch 1],
      expected := [.acquired, .refused, .refused, .refused]}
  , {name := "cancelled_before_restore_selection_closes_known_untouched_operation",
      inputs := reserveInputs ++ [.cancel 1, .advance 1 (.startRestore 0 false true),
        .finishWrite 1 durable, .release 1, .acquire 2 .acquired, .load 2 .empty, admit 2 "new"],
      expected := accepted ++ [.cancelled, .checkpoint, .acknowledged, .released, .acquired, .loaded, .checkpoint]}
  , {name := "cancelled_after_pending_marker_before_launch_records_known_rejection",
      inputs := reserveInputs ++ [.advance 1 (.startRestore 0 true true), .finishWrite 1 durable,
        .cancel 1, .advance 1 (.observeRestore 0 .rejected false true), .finishWrite 1 durable,
        .release 1, .acquire 2 .acquired, .load 2 .empty, admit 2 "new"],
      expected := accepted ++ [.checkpoint, .acknowledged, .cancelled, .checkpoint,
        .acknowledged, .released, .acquired, .loaded, .checkpoint]}
  , {name := "final_preflight_withdrawal_records_known_rejection_without_launch",
      inputs := reserveInputs ++ [.advance 1 (.startRestore 0 true true), .finishWrite 1 durable,
        .withdraw 1, .launch 1, .advance 1 (.observeRestore 0 .rejected false true),
        .finishWrite 1 durable, .release 1],
      expected := accepted ++ [.checkpoint, .acknowledged, .withdrawn, .refused,
        .checkpoint, .acknowledged, .released]}
  , {name := "duplicate_foreign_and_inflight_checkpoint_withdrawal_refuse",
      inputs := reserveInputs ++ [.advance 1 (.startRestore 0 true true), .withdraw 1,
        .finishWrite 1 durable, .withdraw 2, .withdraw 1, .withdraw 1, .launch 1],
      expected := accepted ++ [.checkpoint, .refused, .acknowledged, .refused,
        .withdrawn, .refused, .refused]}
  , {name := "withdrawal_during_actual_restore_promise_refuses", inputs := reserveInputs ++ startRestore ++
      [.withdraw 1, .release 1, .settle 1],
      expected := accepted ++ [.checkpoint, .acknowledged, .effect .restore, .refused, .held, .settled]}
  , {name := "helper_loss_after_unused_grant_withdrawal_still_blocks_workspace",
      inputs := reserveInputs ++ [.advance 1 (.startRestore 0 true true), .finishWrite 1 durable,
        .withdraw 1, .helperLost 1 false, .acquire 2 .acquired, .load 2 .empty, differentAdmission false],
      expected := accepted ++ [.checkpoint, .acknowledged, .withdrawn, .helperLost, .acquired, .refused, .refused]}
  , {name := "observed_auto_save_withdraws_manual_grant_and_continues_remaining_file",
      inputs := [.acquire 1 .acquired, .load 1 .empty, twoAdmission, .finishWrite 1 durable] ++ restored ++
        [.advance 1 (.startSave 0 saveObservations true), .finishWrite 1 durable, .withdraw 1,
         .advance 1 (.observeSave 0 ⟨.saved, true⟩ true), .finishWrite 1 durable,
         .advance 1 (.startRestore 1 true true), .finishWrite 1 durable, .launch 1, .settle 1,
         .advance 1 (.observeRestore 1 .applied true true), .finishWrite 1 durable,
         .advance 1 (.startSave 1 saveObservations true), .finishWrite 1 durable, .launch 1, .settle 1,
         .advance 1 (.observeSave 1 ⟨.saved, true⟩ true), .finishWrite 1 durable, .release 1],
      expected := accepted ++ restoreOutputs ++ [.checkpoint, .acknowledged, .withdrawn, .checkpoint, .acknowledged] ++
        restoreOutputs ++ saveOutputs ++ [.released]}
  ]

def trace (world : World) : List Input → List (World × Output)
  | [] => []
  | input :: rest =>
    let next := Checkpoint.step world input
    next :: trace next.1 rest

theorem expected_traces :
    cases.all (fun c => (trace c.initial c.inputs).map Prod.snd == c.expected) = true := by native_decide

private def boolJson (value : Bool) := if value then "true" else "false"
private def object (fields : List (String × String)) :=
  "{" ++ String.intercalate "," (fields.map fun (key, value) => jsonString key ++ ":" ++ value) ++ "}"
private def stateJson : State → String
  | .fresh => "\"fresh\"" | .pending => "\"pending\"" | .applied => "\"applied\""
  | .rejected => "\"rejected\"" | .unknown => "\"unknown\""
private def saveStateJson : SaveState → String
  | .pending => "\"pending\"" | .saving => "\"saving\"" | .saved => "\"saved\""
  | .conflict => "\"conflict\"" | .failed => "\"failed\"" | .unknown => "\"unknown\""
private def saveJson (marker : SaveMarker) :=
  object [("state", saveStateJson marker.state), ("exact", boolJson marker.exact)]
private def effectJson : Effect → String
  | .none => "\"none\"" | .restore => "\"restore\"" | .save => "\"save\""
private def sequenceJson (sequence : Sequence) := object
  [("items", jsonArray (sequence.items.map fun progress => object
    [("reference", jsonString progress.reference), ("actions", jsonArray (progress.actions.map jsonString)),
     ("uri", jsonString progress.uri), ("restore", stateJson progress.restore), ("save", saveJson progress.save)])),
   ("closed", boolJson sequence.closed)]
private def ledgerJson (ledger : Ledger) := jsonArray (ledger.map fun operation =>
  object [("id", jsonString operation.id), ("sequence", sequenceJson operation.sequence)])
private def storedJson : Stored → String
  | .absent => object [("kind", "\"absent\"")]
  | .present version ledger => object [("kind", "\"present\""), ("version", toString version), ("ledger", ledgerJson ledger)]
  | .corrupt => object [("kind", "\"corrupt\"")]
  | .unavailable => object [("kind", "\"unavailable\"")]
private def proposalJson (proposal : Proposal) := object
  [("ledger", ledgerJson proposal.ledger), ("effect", effectJson proposal.effect),
   ("operation", (proposal.operation.map jsonString).getD "null")]
private def holderJson (holder : Holder) := object
  [("caller", toString holder.caller), ("loaded", boolJson holder.loaded),
   ("refused", boolJson holder.refused), ("cancelled", boolJson holder.cancelled),
   ("ledger", ledgerJson holder.ledger), ("operation", (holder.operation.map jsonString).getD "null"),
   ("pending", (holder.pending.map proposalJson).getD "null"),
   ("grant", effectJson holder.grant), ("running", effectJson holder.running)]
private def worldJson (world : World) := object
  [("stored", storedJson world.stored), ("holder", (world.holder.map holderJson).getD "null"),
   ("orphans", jsonArray (world.orphans.map fun (caller, effect) =>
     object [("caller", toString caller), ("effect", effectJson effect)]))]
private def outputJson : Output → String
  | .acquired => "\"acquired\"" | .busy => "\"busy\"" | .loaded => "\"loaded\""
  | .checkpoint => "\"checkpoint\"" | .acknowledged => "\"acknowledged\"" | .refused => "\"refused\""
  | .effect effect => object [("effect", effectJson effect)]
  | .settled => "\"settled\"" | .cancelled => "\"cancelled\"" | .held => "\"held\"" | .released => "\"released\""
  | .withdrawn => "\"withdrawn\""
  | .helperLost => "\"helperLost\"" | .orphanSettled => "\"orphanSettled\""
private def targetJson (t : Target) := object
  [("reference", jsonString t.reference), ("actions", jsonArray (t.actions.map jsonString)), ("uri", jsonString t.uri),
   ("before", jsonString t.before), ("after", jsonString t.after), ("version", toString t.version),
   ("encoding", jsonString t.encoding), ("eol", jsonString t.eol)]
private def documentJson (d : Document) := object
  [("uri", jsonString d.uri), ("content", jsonString d.content), ("version", toString d.version),
   ("encoding", jsonString d.encoding), ("eol", jsonString d.eol), ("dirty", boolJson d.dirty),
   ("closed", boolJson d.closed), ("writable", boolJson d.writable)]
private def observationsJson (o : Observations) := object
  [("trusted", boolJson o.trusted), ("selectionCurrent", boolJson o.selectionCurrent),
   ("sourcesVerified", boolJson o.sourcesVerified), ("mappingsCurrent", boolJson o.mappingsCurrent),
   ("preservesConflicts", boolJson o.preservesConflicts)]
private def saveObservationsJson (o : SaveObservations) := object
  [("preservesConflicts", boolJson o.preservesConflicts), ("documentCurrent", boolJson o.documentCurrent),
   ("diskPostimageCurrent", boolJson o.diskPostimageCurrent), ("bufferRecoverable", boolJson o.bufferRecoverable)]
private def actionJson : Action → String
  | .startRestore index allowed persisted => object
    [("kind", "\"startRestore\""), ("index", toString index), ("allowed", boolJson allowed), ("persisted", boolJson persisted)]
  | .observeRestore index completion observed persisted => object
    [("kind", "\"observeRestore\""), ("index", toString index), ("completion", stateJson (match completion with
       | .applied => .applied | .rejected => .rejected | .unknown => .unknown)),
     ("observed", boolJson observed), ("persisted", boolJson persisted)]
  | .startSave index o persisted => object
    [("kind", "\"startSave\""), ("index", toString index), ("observations", saveObservationsJson o), ("persisted", boolJson persisted)]
  | .observeSave index result persisted => object
    [("kind", "\"observeSave\""), ("index", toString index), ("result", saveJson result), ("persisted", boolJson persisted)]
  | .observeSavedByEditor index result persisted => object
    [("kind", "\"observeSavedByEditor\""), ("index", toString index), ("result", saveJson result), ("persisted", boolJson persisted)]
private def inputJson : Input → String
  | .acquire caller observation => object [("kind", "\"acquire\""), ("caller", toString caller),
      ("observation", jsonString (match observation with | .acquired => "acquired" | .busy => "busy" | .unavailable => "unavailable"))]
  | .load caller legacy => object [("kind", "\"load\""), ("caller", toString caller),
      ("legacy", jsonString (match legacy with | .empty => "empty" | .records => "records" | .unavailable => "unavailable"))]
  | .admit caller id targets documents o => object [("kind", "\"admit\""), ("caller", toString caller),
      ("id", jsonString id), ("targets", jsonArray (targets.map targetJson)),
      ("documents", jsonArray (documents.map documentJson)), ("observations", observationsJson o)]
  | .advance caller action => object [("kind", "\"advance\""), ("caller", toString caller), ("action", actionJson action)]
  | .finishWrite caller receipt => object [("kind", "\"finishWrite\""), ("caller", toString caller),
      ("receipt", object [("installed", boolJson receipt.installed), ("durable", boolJson receipt.durable),
       ("acknowledged", boolJson receipt.acknowledged)])]
  | .launch caller => object [("kind", "\"launch\""), ("caller", toString caller)]
  | .withdraw caller => object [("kind", "\"withdraw\""), ("caller", toString caller)]
  | .settle caller => object [("kind", "\"settle\""), ("caller", toString caller)]
  | .cancel caller => object [("kind", "\"cancel\""), ("caller", toString caller)]
  | .release caller => object [("kind", "\"release\""), ("caller", toString caller)]
  | .helperLost caller installed => object [("kind", "\"helperLost\""), ("caller", toString caller), ("candidateInstalled", boolJson installed)]
  | .settleOrphan caller => object [("kind", "\"settleOrphan\""), ("caller", toString caller)]
private def traceJson (world : World) : List Input → List String
  | [] => []
  | input :: rest =>
    let next := Checkpoint.step world input
    object [("world", worldJson world), ("input", inputJson input),
      ("expected", worldJson next.1), ("output", outputJson next.2)] :: traceJson next.1 rest

def casesJson := jsonArray (cases.map fun c => object
  [("name", jsonString c.name), ("initial", worldJson c.initial), ("trace", jsonArray (traceJson c.initial c.inputs))])
end Conformance.EditorUndoCheckpoint

def main : IO Unit := IO.println Conformance.EditorUndoCheckpoint.casesJson
