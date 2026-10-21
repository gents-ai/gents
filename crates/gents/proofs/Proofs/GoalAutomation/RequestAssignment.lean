import Proofs.GoalAutomation.RequestAncestry

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

/-- A native causal frontier is required for each physical assignment. A fork,
missing parent, cyclic history, or duplicate revision is unknown; a greater
height or later timestamp cannot choose a winner. -/
def latestVersions (versions : List Version) : Option (List Version) :=
  if !historyValid versions then none
  else
    let frontier := versions.filter fun v => !(versions.any fun later =>
      sameAssignment v later && v.commit != later.commit &&
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
