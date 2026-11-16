import Proofs.Automation.GoalAutomation.HistoryProjection
import Proofs.Automation.GoalAutomation.RequestAssignment
import Proofs.ToolCalls.Recovery.Sweeps.SessionMessage
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family
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

example : inputCases.length = 12 := by decide

def freshSessionCases : List ResubmissionInput :=
  [none, some .agentNew, some .pluginJob].flatMap fun creation =>
    [none, some ⟨1, none⟩, some ⟨1, some 100⟩].flatMap fun reference =>
      [false, true].flatMap fun queued =>
        [false, true].map fun continuation => ⟨creation, reference, queued, continuation, 42⟩

example : freshSessionCases.length = 36 := by decide

structure MessageInputCase where
  name : String
  facts : MessageInputFacts
  deriving DecidableEq, Repr

def messageInputCases : List MessageInputCase :=
  [ ⟨"new_session_does_not_inherit_caller_goal", ⟨1, 10, false, false, none, some ⟨9, some 900⟩⟩⟩
  , ⟨"new_session_ignores_orphan_goal", ⟨1, 10, false, true, some ⟨1, 10, 2, some 200⟩, some ⟨9, some 900⟩⟩⟩
  , ⟨"local_target_without_goal", ⟨1, 10, true, true, none, some ⟨9, some 900⟩⟩⟩
  , ⟨"local_target_uses_own_goal", ⟨1, 10, true, true, some ⟨1, 10, 2, some 200⟩, some ⟨9, some 900⟩⟩⟩
  , ⟨"legacy_target_has_no_physical_binding", ⟨1, 10, true, true, some ⟨1, 10, 2, none⟩, some ⟨9, some 900⟩⟩⟩
  ]

example : messageInputCases.length = 5 := by decide

def assignmentJson (a : Assignment) : Json :=
  Json.mkObj [("owner", toJson a.owner), ("session", toJson a.session),
    ("goal", toJson a.goal), ("root", toJson a.root)]

def entryJson (e : Entry) : Json :=
  Json.mkObj [("doc", toJson e.doc), ("owner", toJson e.owner),
    ("session", toJson e.session),
    ("reference", e.reference.map referenceJson |>.getD Json.null),
    ("signature_valid", toJson e.signatureValid)]

def queueScopeJson : QueueScope → Json
  | .unassigned => Json.mkObj [("kind", toJson "unassigned")]
  | .unresolved entry => Json.mkObj [("kind", toJson "unresolved"), ("entry", toJson entry)]
  | .referenced goal root => Json.mkObj [("kind", toJson "referenced"),
      ("goal", toJson goal), ("root", toJson root)]

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

def historyHandoffReason (observation : HistoryObservation) : Option String :=
  let handoff : Recovery.AssignmentHandoff := match observation with
    | .unassigned => .absent
    | .unknown => .unknown
    | .assigned _ _ .complete => .complete
    | .assigned _ _ .stopped => .stopped
    | .assigned _ _ status => .live status.toDefraDB
  Recovery.sessionMessageHandoffReason true handoff

deriving instance ToJson for Goals.Status, Reference, Assignment, Entry, Version, HistoryCase

structure ObservationWire where
  kind : String
  goal : Option Nat
  root : Option Nat
  status : Option Goals.Status
  deriving ToJson

def ObservationWire.ofModel : HistoryObservation → ObservationWire
  | .unassigned => ⟨"unassigned", none, none, none⟩
  | .unknown => ⟨"unknown", none, none, none⟩
  | .assigned goal root status => ⟨"assigned", some goal, some root, some status⟩

/-- The assignment `latest_assignment_goals` and `observe_request_assignment`
derive from a retained version history, over every history case. -/
@[conformance_family]
def history : Family HistoryCase ObservationWire where
  inputs := historyCases
  model := fun c => ObservationWire.ofModel (observeHistory c.versions c.entry)

structure PersistedObservation where
  observation : ObservationWire
  handoffReason : Option String
  deriving ToJson

/-- The histories a served node can hold as written: each document's versions
are written in order by create and update, so a history whose commits fork,
merge, repeat or name a missing parent cannot be persisted. These cases drive
`load_request_assignment`, its in-transaction form and the completion handoff
over native DefraDB history. -/
def persistedHistoryCases : List HistoryCase :=
  historyCases.filter fun c => c.name ∈
    ["retired_goal_keeps_its_status", "reused_goal_id_keeps_old_assignment",
      "later_complete_goal_cannot_complete_older_active_assignment"]

@[conformance_family]
def persistedHistory : Family HistoryCase PersistedObservation where
  inputs := persistedHistoryCases
  model := fun c =>
    { observation := ObservationWire.ofModel (observeHistory c.versions c.entry)
      handoffReason := historyHandoffReason (observeHistory c.versions c.entry) }

deriving instance ToJson for Case

structure AssignmentObservationWire where
  kind : String
  goal : Option Nat
  root : Option Nat
  deriving ToJson

def AssignmentObservationWire.ofModel : Observation → AssignmentObservationWire
  | .unassigned => ⟨"unassigned", none, none⟩
  | .unknown => ⟨"unknown", none, none⟩
  | .assigned goal root => ⟨"assigned", some goal, some root⟩

structure QueueScopeWire where
  kind : String
  entry : Option Nat
  goal : Option Nat
  root : Option Nat
  deriving ToJson

def QueueScopeWire.ofModel : QueueScope → QueueScopeWire
  | .unassigned => ⟨"unassigned", none, none, none⟩
  | .unresolved entry => ⟨"unresolved", some entry, none, none⟩
  | .referenced goal root => ⟨"referenced", none, some goal, some root⟩

structure SignedBindingOutput where
  member : Bool
  observation : AssignmentObservationWire
  queueScope : Option QueueScopeWire
  deriving ToJson

/-- Membership by `request_belongs_to_goal`, the observation by
`observe_request_assignment` and the queue scope by `request_assignment_scope`
over the observed Goals, for every signed assignment case. -/
@[conformance_family]
def signedBinding : Family Case SignedBindingOutput where
  inputs := cases
  model := fun c =>
    { member := belongs c.assignment c.entry
      observation := AssignmentObservationWire.ofModel (observe c.observed c.entry)
      queueScope := (queueScope c.observed c.entry).map QueueScopeWire.ofModel }

/-- The scope `load_request_assignment_scopes` resolves when retained Goal
history cannot be read: only a verified signed reference, with no historical
entry to fall back on. -/
@[conformance_family]
def unreadHistoryScope : Family Case (Option QueueScopeWire) where
  inputs := cases
  model := fun c => (preferSignedQueueScope [] (some c.entry) none).map QueueScopeWire.ofModel

structure MessageInputWire where
  name : String
  owner : Nat
  session : Nat
  sessionReadable : Bool
  observationVerified : Bool
  observed : Option Assignment
  callerReference : Option Reference
  deriving ToJson

structure PreparedWire where
  kind : String
  reference : Option Reference
  deriving ToJson

def PreparedWire.ofModel : PreparedInput → PreparedWire
  | .pending => ⟨"pending", none⟩
  | .prepared reference => ⟨"prepared", reference⟩

/-- The goal assignment `target_goal_assignment` selects for a session message,
which planning signs and staging persists, over every message input case. -/
@[conformance_family]
def messageInput : Family MessageInputWire PreparedWire where
  inputs := messageInputCases.map fun c =>
    { name := c.name, owner := c.facts.owner, session := c.facts.session
      sessionReadable := c.facts.sessionReadable
      observationVerified := c.facts.observationVerified
      observed := c.facts.observed, callerReference := c.facts.callerReference }
  model := fun i => PreparedWire.ofModel (prepareMessageInput
    ⟨i.owner, i.session, i.sessionReadable, i.observationVerified, i.observed, i.callerReference⟩)

example : messageInputCases.all (·.facts.owner == 1) := by decide

example : historyCases.length = 21 := by decide
example : (historyCases.map fun c => observeHistory c.versions c.entry).take 2 =
    [.assigned 1 100 .stopped, .assigned 1 100 .stopped] := by native_decide

structure InputBindingInput where
  name : String
  captured : Option Reference
  sessionReadable : Bool
  observationVerified : Bool
  observed : Option Reference
  deriving ToJson

structure InputBindingOutput where
  kind : String
  reference : Option Reference
  deriving ToJson

@[conformance_family]
def inputBinding : Family InputBindingInput InputBindingOutput where
  inputs := inputCases.map fun c =>
    { name := c.name, captured := c.facts.captured, sessionReadable := c.facts.sessionReadable,
      observationVerified := c.facts.observationVerified, observed := c.facts.observed }
  model := fun i =>
    match prepareInput ⟨i.captured, i.sessionReadable, i.observationVerified, i.observed⟩ with
    | .pending => { kind := "pending", reference := none }
    | .prepared reference => { kind := "prepared", reference }

deriving instance ToJson for CreationIntent, ResubmissionInput

structure FreshSessionCase where
  input : ResubmissionInput
  deriving ToJson

structure FreshSessionResult where
  expected : ResubmissionInput
  deriving ToJson

@[conformance_family]
def freshSession : Family FreshSessionCase FreshSessionResult where
  inputs := freshSessionCases.map fun input => { input }
  model := fun c => { expected := freshSessionInput c.input }

end Conformance.GoalRequestAssignmentContracts
