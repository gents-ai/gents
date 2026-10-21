import Proofs.GoalAutomation.RequestAssignment
import Proofs.Recovery.Sweeps.SessionMessage
import Proofs.Conformance.ContractTypes
import Lean

namespace Conformance.GoalRequestAssignmentContracts

open GoalAutomation.RequestAssignment Lean

structure Case where
  name : String
  assignment : Assignment := ⟨1, 10, 1, some 100⟩
  entry : Entry := ⟨100, 1, 10, some ⟨1, none⟩, true⟩
  observed : List Assignment := [⟨1, 10, 2, some 200⟩, ⟨1, 10, 1, some 100⟩]
  deriving DecidableEq, Repr

def cases : List Case :=
  [ { name := "first_request_binds_itself" }
  , { name := "ordinary_followup_keeps_goal", entry := ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩ }
  , { name := "later_goal_does_not_capture_old_followup", assignment := ⟨1, 10, 2, some 200⟩,
      entry := ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩ }
  , { name := "reused_goal_id_retains_old_assignment", assignment := ⟨1, 10, 1, some 200⟩,
      entry := ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩,
      observed := [⟨1, 10, 1, some 200⟩, ⟨1, 10, 1, some 100⟩] }
  , { name := "current_followup_joins_new_assignment", assignment := ⟨1, 10, 2, some 200⟩,
      entry := ⟨201, 1, 10, some ⟨2, some 200⟩, true⟩ }
  , { name := "task_opening_request_uses_physical_binding", entry := ⟨100, 1, 10, none, true⟩ }
  , { name := "unbound_followup_cannot_join_pinned_goal", entry := ⟨101, 1, 10, none, true⟩ }
  , { name := "self_reference_cannot_impersonate_another_root", entry := ⟨101, 1, 10, some ⟨1, none⟩, true⟩ }
  , { name := "other_goal_same_root_cannot_join", entry := ⟨100, 1, 10, some ⟨2, some 100⟩, true⟩ }
  , { name := "unknown_reference_is_not_unassigned", entry := ⟨101, 1, 10, some ⟨9, some 900⟩, true⟩ }
  , { name := "wrong_owner", entry := ⟨101, 2, 10, some ⟨1, some 100⟩, true⟩ }
  , { name := "wrong_session", entry := ⟨101, 1, 11, some ⟨1, some 100⟩, true⟩ }
  , { name := "invalid_signature", entry := ⟨101, 1, 10, some ⟨1, some 100⟩, false⟩ }
  , { name := "unbound_legacy_head_keeps_existing_membership", assignment := ⟨1, 10, 1, none⟩,
      entry := ⟨100, 1, 10, none, true⟩, observed := [⟨1, 10, 1, none⟩] }
  , { name := "tagged_request_cannot_join_unbound_legacy_goal", assignment := ⟨1, 10, 1, none⟩ }
  , { name := "absent_goals_have_no_assignment", entry := ⟨100, 1, 10, none, true⟩, observed := [] }
  , { name := "missing_referenced_goal_is_unknown", observed := [] }
  , { name := "ambiguous_physical_binding_is_unknown", entry := ⟨100, 1, 10, none, true⟩,
      observed := [⟨1, 10, 1, some 100⟩, ⟨1, 10, 2, some 100⟩] }
  ]

def referenceJson (reference : Reference) : Json :=
  Json.mkObj [("goal", toJson reference.goal), ("root", toJson reference.root)]

structure InputCase where
  name : String
  facts : InputFacts
  deriving DecidableEq, Repr

def inputCases : List InputCase :=
  [ ⟨"fresh_chat", ⟨none, false, false, none⟩⟩
  , ⟨"fresh_chat_ignores_unscoped_observation", ⟨none, false, true, some ⟨2, some 200⟩⟩⟩
  , ⟨"existing_chat_waits_for_observation", ⟨none, true, false, none⟩⟩
  , ⟨"unverified_absence_is_unknown", ⟨none, true, false, none⟩⟩
  , ⟨"unverified_goal_is_unknown", ⟨none, true, false, some ⟨2, some 200⟩⟩⟩
  , ⟨"verified_absence_is_unassigned", ⟨none, true, true, none⟩⟩
  , ⟨"legacy_goal_cannot_supply_a_root", ⟨none, true, true, some ⟨1, none⟩⟩⟩
  , ⟨"followup_inherits_current_assignment", ⟨none, true, true, some ⟨2, some 200⟩⟩⟩
  , ⟨"captured_old_assignment_survives_replacement",
      ⟨some ⟨1, some 100⟩, true, true, some ⟨2, some 200⟩⟩⟩
  , ⟨"captured_opening_request_keeps_self_reference", ⟨some ⟨2, none⟩, true, false, none⟩⟩
  , ⟨"captured_reference_survives_unknown_state", ⟨some ⟨1, some 100⟩, true, false, none⟩⟩
  , ⟨"captured_reference_does_not_need_session_read", ⟨some ⟨1, some 100⟩, false, false, none⟩⟩
  ]

def inputCaseJson (c : InputCase) : Json :=
  let expected := match prepareInput c.facts with
    | .pending => Json.mkObj [("kind", toJson "pending")]
    | .prepared reference => Json.mkObj [("kind", toJson "prepared"),
        ("reference", reference.map referenceJson |>.getD Json.null)]
  Json.mkObj [("name", toJson c.name),
    ("captured", c.facts.captured.map referenceJson |>.getD Json.null),
    ("session_readable", toJson c.facts.sessionReadable),
    ("observation_verified", toJson c.facts.observationVerified),
    ("observed", c.facts.observed.map referenceJson |>.getD Json.null),
    ("expected", expected)]

def inputCasesJson : String := (toJson (inputCases.map inputCaseJson)).compress

example : inputCases.length = 12 := by decide

def resubmissionInputJson (input : ResubmissionInput) : Json :=
  Json.mkObj [("reference", input.reference.map referenceJson |>.getD Json.null),
    ("queued", toJson input.queued), ("continuation", toJson input.continuation),
    ("authored", toJson input.authored)]

def freshSessionCases : List ResubmissionInput :=
  [none, some ⟨1, none⟩, some ⟨1, some 100⟩].flatMap fun reference =>
    [false, true].flatMap fun queued =>
      [false, true].map fun continuation => ⟨reference, queued, continuation, 42⟩

def freshSessionCasesJson : String :=
  (toJson (freshSessionCases.map fun input => Json.mkObj [
    ("input", resubmissionInputJson input),
    ("expected", resubmissionInputJson (freshSessionInput input))])).compress

example : freshSessionCases.length = 12 := by decide

structure MessageInputCase where
  name : String
  facts : MessageInputFacts
  deriving DecidableEq, Repr

def messageInputCases : List MessageInputCase :=
  [ ⟨"new_session_does_not_inherit_caller_goal", ⟨1, 10, false, false, none, some ⟨9, some 900⟩⟩⟩
  , ⟨"new_session_ignores_orphan_goal", ⟨1, 10, false, true, some ⟨1, 10, 2, some 200⟩, some ⟨9, some 900⟩⟩⟩
  , ⟨"local_target_without_goal", ⟨1, 10, true, true, none, some ⟨9, some 900⟩⟩⟩
  , ⟨"local_target_uses_own_goal", ⟨1, 10, true, true, some ⟨1, 10, 2, some 200⟩, some ⟨9, some 900⟩⟩⟩
  , ⟨"remote_target_uses_verified_goal", ⟨2, 10, true, true, some ⟨2, 10, 2, some 200⟩, some ⟨9, some 900⟩⟩⟩
  , ⟨"remote_verified_absence", ⟨2, 10, true, true, none, some ⟨9, some 900⟩⟩⟩
  , ⟨"legacy_target_has_no_physical_binding", ⟨1, 10, true, true, some ⟨1, 10, 2, none⟩, some ⟨9, some 900⟩⟩⟩
  , ⟨"remote_missing_observation_waits", ⟨2, 10, true, false, none, some ⟨9, some 900⟩⟩⟩
  , ⟨"remote_forged_observation_waits", ⟨2, 10, true, false, some ⟨2, 10, 2, some 200⟩, some ⟨9, some 900⟩⟩⟩
  , ⟨"remote_foreign_owner_waits", ⟨2, 10, true, true, some ⟨1, 10, 2, some 200⟩, some ⟨9, some 900⟩⟩⟩
  , ⟨"remote_foreign_session_waits", ⟨2, 10, true, true, some ⟨2, 11, 2, some 200⟩, some ⟨9, some 900⟩⟩⟩
  ]

def messageInputCaseJson (c : MessageInputCase) : Json :=
  let expected := match prepareMessageInput c.facts with
    | .pending => Json.mkObj [("kind", toJson "pending")]
    | .prepared reference => Json.mkObj [("kind", toJson "prepared"),
        ("reference", reference.map referenceJson |>.getD Json.null)]
  let observed := c.facts.observed.map fun a => Json.mkObj [
    ("owner", toJson a.owner), ("session", toJson a.session),
    ("goal", toJson a.goal), ("root", toJson a.root)]
  Json.mkObj [("name", toJson c.name), ("owner", toJson c.facts.owner),
    ("session", toJson c.facts.session), ("session_readable", toJson c.facts.sessionReadable),
    ("observation_verified", toJson c.facts.observationVerified),
    ("observed", observed.getD Json.null),
    ("caller_reference", c.facts.callerReference.map referenceJson |>.getD Json.null),
    ("expected", expected)]

def messageInputCasesJson : String := (toJson (messageInputCases.map messageInputCaseJson)).compress

example : messageInputCases.length = 11 := by decide

def assignmentJson (a : Assignment) : Json :=
  Json.mkObj [("owner", toJson a.owner), ("session", toJson a.session),
    ("goal", toJson a.goal), ("root", toJson a.root)]

def entryJson (e : Entry) : Json :=
  Json.mkObj [("doc", toJson e.doc), ("owner", toJson e.owner),
    ("session", toJson e.session),
    ("reference", e.reference.map referenceJson |>.getD Json.null),
    ("signature_valid", toJson e.signatureValid)]

def observationJson : Observation → Json
  | .unassigned => Json.mkObj [("kind", toJson "unassigned")]
  | .unknown => Json.mkObj [("kind", toJson "unknown")]
  | .assigned goal root => Json.mkObj [("kind", toJson "assigned"),
      ("goal", toJson goal), ("root", toJson root)]

def queueScopeJson : QueueScope → Json
  | .unassigned => Json.mkObj [("kind", toJson "unassigned")]
  | .unresolved entry => Json.mkObj [("kind", toJson "unresolved"), ("entry", toJson entry)]
  | .referenced goal root => Json.mkObj [("kind", toJson "referenced"),
      ("goal", toJson goal), ("root", toJson root)]

def caseJson (c : Case) : Json :=
  Json.mkObj [("name", toJson c.name), ("assignment", assignmentJson c.assignment),
    ("entry", entryJson c.entry), ("observed", toJson (c.observed.map assignmentJson)),
    ("expected_member", toJson (belongs c.assignment c.entry)),
    ("expected_observation", observationJson (observe c.observed c.entry)),
    ("expected_queue_scope", (queueScope c.observed c.entry).map queueScopeJson |>.getD Json.null)]

def casesJson : String := (toJson (cases.map caseJson)).compress

example : cases.length = 18 := by decide

structure HistoryCase where
  name : String
  versions : List Version
  entry : Entry := ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩
  retireDocuments : List Nat := []
  deriving DecidableEq, Repr

def historyCases : List HistoryCase :=
  let first : Version := ⟨10, 1, 1, [], ⟨1, 10, 1, some 100⟩, .active⟩
  let stopped := {first with commit := 2, height := 2, parents := [1], status := .stopped}
  let fresh := {first with document := 20, commit := 3, assignment := ⟨1, 10, 2, some 200⟩}
  let reassigned := {first with commit := 3, height := 3, parents := [2], assignment := ⟨1, 10, 1, some 200⟩}
  let fork := {stopped with commit := 3, height := 4, status := .complete}
  [ {name := "retired_goal_keeps_its_status", versions := [first, stopped, fresh], retireDocuments := [10]}
  , {name := "reused_goal_id_keeps_old_assignment", versions := [first, stopped, reassigned]}
  , {name := "later_complete_goal_cannot_complete_older_active_assignment",
      versions := [first, {reassigned with height := 2, parents := [1], status := .complete}]}
  , {name := "current_followup_uses_current_assignment", versions := [first, stopped, fresh],
      entry := ⟨201, 1, 10, some ⟨2, some 200⟩, true⟩}
  , {name := "completed_old_assignment_remains_complete",
      versions := [first, {stopped with status := .complete}, reassigned]}
  , {name := "concurrent_states_are_unknown_despite_different_heights", versions := [first, stopped, fork]}
  , {name := "merged_native_frontier_resolves_assignment", versions := [first, stopped, fork,
      {first with commit := 4, height := 5, parents := [2, 3], status := .complete}]}
  , {name := "missing_parent_is_unknown", versions := [first, {stopped with parents := [9]}]}
  , {name := "duplicate_revision_is_unknown", versions := [first, stopped, {stopped with status := .complete}]}
  , {name := "cyclic_history_is_unknown", versions := [{first with parents := [2]}, stopped]}
  , {name := "foreign_owner_cannot_claim_assignment", versions := [first, stopped],
      entry := ⟨101, 2, 10, some ⟨1, some 100⟩, true⟩}
  , {name := "untagged_task_root_keeps_retired_assignment", versions := [first, stopped, reassigned],
      entry := ⟨100, 1, 10, none, true⟩}
  , {name := "no_goal_is_unassigned", versions := [], entry := ⟨100, 1, 10, none, true⟩}
  , {name := "unbound_history_cannot_prove_assignment", versions := [{first with assignment := ⟨1, 10, 1, none⟩}],
      entry := ⟨100, 1, 10, none, true⟩}
  , {name := "another_document_cannot_supply_missing_parent", versions := [first,
      {fresh with parents := [1], height := 2}]}
  , {name := "physical_documents_keep_their_own_commit_scope", versions := [first,
      {fresh with commit := 1}]}
  , {name := "first_binding_retires_only_its_unbound_placeholder", versions :=
      [{first with assignment := ⟨1,10,1,none⟩}, {first with commit := 2, height := 2, parents := [1]}],
      entry := ⟨101,1,10,none,true⟩}
  , {name := "first_binding_keeps_its_physical_opening", versions :=
      [{first with assignment := ⟨1,10,1,none⟩}, {first with commit := 2, height := 2, parents := [1]}],
      entry := ⟨100,1,10,none,true⟩}
  , {name := "unrelated_unbound_document_stays_unknown", versions :=
      [{first with assignment := ⟨1,10,1,none⟩}, fresh], entry := ⟨101,1,10,none,true⟩}
  , {name := "concurrent_unbound_branch_stays_unknown", versions :=
      [{first with assignment := ⟨1,10,1,none⟩}, {first with commit := 2, height := 2, parents := [1]},
       {first with commit := 3, height := 3, parents := [1], assignment := ⟨1,10,1,none⟩}],
      entry := ⟨101,1,10,none,true⟩}
  , {name := "another_goal_cannot_retire_unbound_identity", versions :=
      [{first with assignment := ⟨1,10,1,none⟩}, {first with commit := 2, height := 2, parents := [1], assignment := ⟨1,10,2,some 100⟩}],
      entry := ⟨101,1,10,none,true⟩}
  ]

def versionJson (v : Version) : Json :=
  Json.mkObj [("document", toJson v.document), ("commit", toJson v.commit),
    ("height", toJson v.height), ("parents", toJson v.parents),
    ("assignment", assignmentJson v.assignment), ("status", toJson v.status.toDefraDB)]

def historyObservationJson : HistoryObservation → Json
  | .unassigned => Json.mkObj [("kind", toJson "unassigned")]
  | .unknown => Json.mkObj [("kind", toJson "unknown")]
  | .assigned goal root status => Json.mkObj [("kind", toJson "assigned"),
      ("goal", toJson goal), ("root", toJson root), ("status", toJson status.toDefraDB)]

def historyHandoffReason (observation : HistoryObservation) : Option String :=
  let handoff : Recovery.AssignmentHandoff := match observation with
    | .unassigned => .absent
    | .unknown => .unknown
    | .assigned _ _ .complete => .complete
    | .assigned _ _ .stopped => .stopped
    | .assigned _ _ status => .live status.toDefraDB
  Recovery.sessionMessageHandoffReason true handoff

def historyCaseJson (c : HistoryCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("versions", toJson (c.versions.map versionJson)),
    ("entry", entryJson c.entry), ("retire_documents", toJson c.retireDocuments),
    ("expected", historyObservationJson (observeHistory c.versions c.entry)),
    ("expected_handoff_reason", toJson (historyHandoffReason (observeHistory c.versions c.entry)))]

def historyCasesJson : String := (toJson (historyCases.map historyCaseJson)).compress

example : historyCases.length = 21 := by decide
example : (historyCases.map fun c => observeHistory c.versions c.entry).take 2 =
    [.assigned 1 100 .stopped, .assigned 1 100 .stopped] := by native_decide

end Conformance.GoalRequestAssignmentContracts
