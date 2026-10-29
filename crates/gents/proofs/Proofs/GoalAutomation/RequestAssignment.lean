import Proofs.GoalAutomation.RequestAncestry
import Mathlib.Data.Nat.Pairing

namespace GoalAutomation.RequestAssignment

/-- A signed invocation names its Goal and the physical opening request.
`none` denotes the invocation itself, avoiding a self-referential document
hash on the first request. Later invocations carry the opening document ID.
This is an association within existing request admission and document ACP,
not an additional grant or identity. -/
structure Reference where
  goal : Nat
  root : Option Nat
  deriving DecidableEq, Repr

structure Assignment where
  owner : Nat
  session : Nat
  goal : Nat
  root : Option Nat
  deriving DecidableEq, Repr

/-- The existing authenticated ancestry owner resolves a candidate to this
entry. Scope and signature facts come from its canonical signed request. -/
structure Entry where
  doc : Nat
  owner : Nat
  session : Nat
  reference : Option Reference
  signatureValid : Bool
  deriving DecidableEq, Repr

structure InputFacts where
  captured : Option Reference
  sessionReadable : Bool
  observationVerified : Bool
  observed : Option Reference
  deriving DecidableEq, Repr

inductive PreparedInput where
  | pending
  | prepared (reference : Option Reference)
  deriving DecidableEq, Repr

/-- A captured reference is immutable intent. Existing chats must wait for a
source-signed observation; missing or unverifiable projection data does not
establish absence. Legacy unbound Goals cannot supply a physical root. -/
def prepareInput (facts : InputFacts) : PreparedInput :=
  match facts.captured with
  | some reference => .prepared (some reference)
  | none =>
      if !facts.sessionReadable then .prepared none
      else if !facts.observationVerified then .pending
      else .prepared (facts.observed.filter fun reference => reference.root.isSome)

theorem captured_input_keeps_its_reference (facts : InputFacts) (reference : Reference)
    (captured : facts.captured = some reference) :
    prepareInput facts = .prepared (some reference) := by
  simp [prepareInput, captured]

theorem unknown_existing_chat_cannot_prepare (facts : InputFacts)
    (uncaptured : facts.captured = none) (existing : facts.sessionReadable = true)
    (unknown : facts.observationVerified = false) : prepareInput facts = .pending := by
  simp [prepareInput, uncaptured, existing, unknown]

theorem fresh_chat_has_no_inherited_assignment (facts : InputFacts)
    (uncaptured : facts.captured = none) (fresh : facts.sessionReadable = false) :
    prepareInput facts = .prepared none := by
  simp [prepareInput, uncaptured, fresh]

/-- A stale resend keeps its authored payload but starts a fresh session.
Queue position, Goal continuation facts and assignment references describe
the original invocation and cannot cross that fresh-session boundary. -/
inductive CreationIntent where
  | agentNew | pluginJob
  deriving DecidableEq, Repr

structure ResubmissionInput where
  creation : Option CreationIntent
  reference : Option Reference
  queued : Bool
  continuation : Bool
  authored : Nat
  deriving DecidableEq, Repr

def freshSessionInput (input : ResubmissionInput) : ResubmissionInput :=
  { input with creation := none, reference := none, queued := false, continuation := false }

theorem fresh_resubmission_is_unassigned (input : ResubmissionInput) :
    (freshSessionInput input).reference = none := by rfl

theorem fresh_resubmission_has_no_invocation_lineage (input : ResubmissionInput) :
    (freshSessionInput input).creation = none ∧
      (freshSessionInput input).queued = false ∧
      (freshSessionInput input).continuation = false := by simp [freshSessionInput]

theorem fresh_resubmission_preserves_authored_payload (input : ResubmissionInput) :
    (freshSessionInput input).authored = input.authored := by rfl

/-- Session-message input belongs to the destination session. Its caller's
Goal is not input capture. A source read is either the destination runtime's
canonical transaction or its verified session-scoped observation. -/
structure MessageInputFacts where
  owner : Nat
  session : Nat
  sessionReadable : Bool
  observationVerified : Bool
  observed : Option Assignment
  callerReference : Option Reference
  deriving DecidableEq, Repr

def prepareSelectedInput (existing : Bool) (captured : Option Reference)
    (facts : MessageInputFacts) : PreparedInput :=
  prepareInput {
    captured
    sessionReadable := existing && facts.sessionReadable
    observationVerified := facts.observationVerified &&
      facts.observed.all (fun a => a.owner == facts.owner && a.session == facts.session)
    observed := facts.observed.map fun a => ⟨a.goal, a.root⟩
  }

def prepareMessageInput (facts : MessageInputFacts) : PreparedInput :=
  prepareSelectedInput true none facts

theorem message_input_ignores_caller_assignment (facts : MessageInputFacts)
    (caller : Option Reference) :
    prepareMessageInput { facts with callerReference := caller } = prepareMessageInput facts := by
  rfl

def inScope (a : Assignment) (e : Entry) : Bool :=
  a.owner == e.owner && a.session == e.session

def belongs (a : Assignment) (e : Entry) : Bool :=
  e.signatureValid && inScope a e && match a.root, e.reference with
  | none, none => true
  | none, some _ => false
  | some root, none => root == e.doc
  | some root, some reference =>
      reference.goal == a.goal && reference.root.getD e.doc == root

inductive Observation where
  | unassigned
  | unknown
  | assigned (goal root : Nat)
  deriving DecidableEq, Repr

/-- A handoff must be tied to a physical assignment. An older unbound Goal
cannot prove which request it owned, so it cannot turn a finished tool turn
into assignment success. Retained versions of the same Goal ID remain distinct
when their physical opening requests differ. -/
def observe (assignments : List Assignment) (e : Entry) : Observation :=
  if !e.signatureValid then .unknown
  else
    let candidates := assignments.filter fun a => a.root.isSome && belongs a e
    match candidates with
    | [a] => match a.root with
      | some root => .assigned a.goal root
      | none => .unknown
    | [] =>
        if e.reference.isSome || assignments.any (fun a => inScope a e && a.root.isNone)
        then .unknown else .unassigned
    | _ => .unknown

/-- A queue scope groups the same signed assignment intent. A retained Goal
can identify an untagged opening; an unbound legacy observation stays isolated
at its authenticated entry. A reference is association, never access authority
or proof that a Goal is present or active. -/
inductive QueueScope where
  | unassigned
  | unresolved (entry : Nat)
  | referenced (goal root : Nat)
  deriving DecidableEq, Repr

/-- Native coalescing compares the original queue key together with its
canonical retained assignment scope. Nat is the existing abstract key identity;
this encoding introduces no persisted field or alternate authorization. -/
def queueScopeKey (base : Nat) (scope : QueueScope) : Nat :=
  let association := match scope with
    | .unassigned => 0
    | .unresolved entry => 2 * entry + 1
    | .referenced goal root => 2 * Nat.pair goal root + 2
  Nat.pair base association

def queueScope (assignments : List Assignment) (e : Entry) : Option QueueScope :=
  if !e.signatureValid then none
  else some (match e.reference with
    | some reference => .referenced reference.goal (reference.root.getD e.doc)
    | none => match observe assignments e with
      | .unassigned => .unassigned
      | .unknown => .unresolved e.doc
      | .assigned goal root => .referenced goal root)

/-- A verified signed reference is association, not a claim about the retained
Goal document. It identifies the same work when history is unavailable or a
retained opening belongs to another assignment. Untagged requests retain the
historical ancestry owner. -/
def preferSignedQueueScope (assignments : List Assignment)
    (signed historical : Option Entry) : Option QueueScope :=
  let direct := signed.bind fun entry =>
    if entry.reference.isSome then queueScope assignments entry else none
  direct.orElse fun _ => historical.bind (queueScope assignments)

def scopedEntry (owner session doc : Nat) (entries : List Entry) : Option Entry :=
  match entries.filter (fun entry => entry.owner == owner && entry.session == session && entry.doc == doc) with
  | [entry] => some entry
  | _ => none

/-- Compose the executable ancestry owners. Signed ancestry authenticates every
continuation edge; retained openings are consulted only when it supplies no
signed association. -/
def queueScopeWithAncestry (owner session : Nat)
    (rows : List RequestAncestry.AssignmentRow) (entries : List Entry)
    (assignments : List Assignment) (doc : Nat) : Option QueueScope :=
  let signed := (RequestAncestry.signedEntry owner session (rows.map (·.request)) none doc).bind
    (fun entry => scopedEntry owner session entry entries)
  let historical := (RequestAncestry.assignmentEntry owner session rows none doc).bind
    (fun entry => scopedEntry owner session entry entries)
  preferSignedQueueScope assignments signed historical

theorem authenticated_reference_does_not_depend_on_history
    (assignments : List Assignment) (entry : Entry) (reference : Reference)
    (historical : Option Entry) (valid : entry.signatureValid = true)
    (tagged : entry.reference = some reference) :
    preferSignedQueueScope assignments (some entry) historical =
      some (.referenced reference.goal (reference.root.getD entry.doc)) := by
  simp [preferSignedQueueScope, queueScope, valid, tagged]

theorem untagged_entry_retains_historical_scope (assignments : List Assignment)
    (entry : Entry) (historical : Option Entry) (untagged : entry.reference = none) :
    preferSignedQueueScope assignments (some entry) historical =
      historical.bind (queueScope assignments) := by
  simp [preferSignedQueueScope, untagged]

theorem unbound_queue_scope_is_not_unassigned (assignments : List Assignment) (entry : Entry)
    (valid : entry.signatureValid = true) (untagged : entry.reference = none)
    (unknown : observe assignments entry = .unknown) :
    queueScope assignments entry = some (.unresolved entry.doc) := by
  simp [queueScope, valid, untagged, unknown]

theorem invalid_signature_cannot_join (a : Assignment) (e : Entry)
    (h : e.signatureValid = false) : belongs a e = false := by
  simp [belongs, h]

theorem foreign_owner_cannot_join (a : Assignment) (e : Entry)
    (h : a.owner ≠ e.owner) : belongs a e = false := by
  simp [belongs, inScope, h]

theorem foreign_session_cannot_join (a : Assignment) (e : Entry)
    (h : a.session ≠ e.session) : belongs a e = false := by
  simp [belongs, inScope, h]

theorem stale_root_cannot_join (a : Assignment) (e : Entry) (reference : Reference)
    (root : Nat) (ha : a.root = some root) (he : e.reference = some reference)
    (h : reference.root.getD e.doc ≠ root) : belongs a e = false := by
  simp [belongs, ha, he, h]

theorem invalid_signature_cannot_claim_assignment (assignments : List Assignment)
    (e : Entry) (h : e.signatureValid = false) : observe assignments e = .unknown := by
  simp [observe, h]

theorem later_assignment_keeps_old_handoff :
    observe [⟨1, 10, 1, some 200⟩, ⟨1, 10, 1, some 100⟩]
      ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩ = .assigned 1 100 := by decide

theorem unbound_legacy_goal_cannot_claim_success :
    observe [⟨1, 10, 1, none⟩] ⟨100, 1, 10, none, true⟩ = .unknown := by decide

/-- Revisions and physical parent links come from DefraDB's composite commits.
Height validates the native DAG; it never orders concurrent assignment states.
Deleted documents remain in this read observation through DocumentACP. -/
structure Version where
  document : Nat
  commit : Nat
  height : Nat
  parents : List Nat
  assignment : Assignment
  status : Goals.Status
  deriving DecidableEq, Repr

def historyValid (versions : List Version) : Bool :=
  decide ((versions.map fun v => (v.document, v.commit)).Nodup) &&
    versions.all fun v => v.parents.all fun parent => versions.any fun p =>
      p.document == v.document && p.commit == parent && p.height < v.height

def reaches (versions : List Version) (document target : Nat) : Nat → Nat → Bool
  | 0, _ => false
  | fuel + 1, current => current == target ||
      ((versions.find? fun v => v.document == document && v.commit == current).map
        fun v => v.parents.any (reaches versions document target fuel)).getD false

def sameAssignment (left right : Version) : Bool :=
  left.document == right.document && decide (left.assignment = right.assignment)

/-- The rootless creation revision is a placeholder until the same physical
Goal receives its first request. Only a causal descendant of the same Goal
identity can retire that placeholder; earlier nonempty roots remain distinct
assignments after Task reassignment. -/
def supersedesAssignment (earlier later : Version) : Bool :=
  earlier.document == later.document &&
    earlier.assignment.owner == later.assignment.owner &&
    earlier.assignment.session == later.assignment.session &&
    earlier.assignment.goal == later.assignment.goal &&
    (earlier.assignment.root == later.assignment.root ||
      earlier.assignment.root.isNone && later.assignment.root.isSome)

/-- A native causal frontier is required for each physical assignment. A fork,
missing parent, cyclic history, or duplicate revision is unknown; a greater
height or later timestamp cannot choose a winner. -/
def latestVersions (versions : List Version) : Option (List Version) :=
  if !historyValid versions then none
  else
    let frontier := versions.filter fun v => !(versions.any fun later =>
      supersedesAssignment v later && v.commit != later.commit &&
        reaches versions v.document v.commit (versions.length + 1) later.commit)
    if (frontier.map fun v => (v.document, v.assignment)).Nodup then some frontier else none

inductive HistoryObservation where
  | unassigned
  | unknown
  | assigned (goal root : Nat) (status : Goals.Status)
  deriving DecidableEq, Repr

def observeHistory (versions : List Version) (entry : Entry) : HistoryObservation :=
  match latestVersions versions with
  | none => .unknown
  | some frontier => match observe (frontier.map Version.assignment) entry with
    | .unassigned => .unassigned
    | .unknown => .unknown
    | .assigned goal root =>
        match frontier.find? fun v => belongs v.assignment entry &&
            v.assignment.goal == goal && v.assignment.root == some root with
        | some v => .assigned goal root v.status
        | none => .unknown

theorem incomplete_history_cannot_claim_assignment (versions : List Version) (entry : Entry)
    (invalid : historyValid versions = false) :
    observeHistory versions entry = .unknown := by
  simp [observeHistory, latestVersions, invalid]

theorem ambiguous_history_cannot_claim_assignment (versions : List Version) (entry : Entry)
    (unknown : latestVersions versions = none) :
    observeHistory versions entry = .unknown := by
  simp [observeHistory, unknown]

end GoalAutomation.RequestAssignment
