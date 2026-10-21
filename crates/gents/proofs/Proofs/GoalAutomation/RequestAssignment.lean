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

end GoalAutomation.RequestAssignment
