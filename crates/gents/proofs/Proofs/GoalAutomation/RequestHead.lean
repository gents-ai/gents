import Proofs.GoalAutomation
import Proofs.Request.State

namespace GoalAutomation.RequestHead

structure Scope where
  owner : Nat
  session : Nat
  goal : Nat
  deriving DecidableEq, Repr

/-- Classification supplied by the existing request lifecycle owner. Folding,
replacement and removal retire authored queue inputs without ending the Goal's
executing request. An explicit execution interrupt remains `execution`. -/
inductive InputDisposition where
  | execution
  | folded
  | replaced
  | removed
  deriving DecidableEq, Repr

structure Row where
  purpose : RequestPurpose
  doc : Nat
  request : Nat
  owner : Nat
  session : Nat
  goal : Option Nat
  parentDoc : Option Nat
  parentRequest : Option Nat
  sequence : Nat
  /-- Results of the existing receipt validator, evaluated only for matching edges. -/
  receiptValid : Bool
  deterministicIdentity : Bool
  inputDisposition : InputDisposition := .execution
  deriving DecidableEq, Repr

def inScope (s : Scope) (r : Row) : Bool :=
  r.purpose == .normal && r.owner == s.owner && r.session == s.session

/-- Cheap qualification/binding checks precede cryptographic validation. The
shared Goal physical-edge helper checks original parent/child signatures,
owner/session, exact physical parent pair, typed signed Goal continuation
facts and original deterministic request/retry identity. Historical omission
of inherited source fields does not erase a signed physical edge. Fresh
typed-resume receipt checks remain stricter; this ordering projection does
not authorize publication. -/
def verifiedEdge (s : Scope) (parent child : Row) : Bool :=
  child.goal == some s.goal && child.parentDoc == some parent.doc &&
  child.parentRequest == some parent.request && child.doc != parent.doc &&
  inScope s parent && inScope s child && child.sequence > 0 &&
  child.receiptValid && child.deterministicIdentity &&
  (parent.goal.isNone || parent.goal == child.goal)

def superseded (s : Scope) (rows : List Row) (parent : Row) : Bool :=
  rows.any (verifiedEdge s parent)

def select (s : Scope) (orderedRows : List Row) : Option Row :=
  let rows := orderedRows.filter (inScope s)
  rows.find? fun row => row.inputDisposition == .execution && !superseded s rows row

theorem title_arrival_preserves_head (s : Scope) (rows : List Row) (title : Row)
    (hp : title.purpose = .titleAudit) : select s (title :: rows) = select s rows := by
  simp [select, inScope, hp]

theorem selected_is_scoped_member (s : Scope) (rows : List Row) (head : Row)
    (h : select s rows = some head) : head ∈ rows.filter (inScope s) :=
  List.mem_of_find?_eq_some h

theorem selected_has_no_verified_child (s : Scope) (rows : List Row) (head : Row)
    (h : select s rows = some head) : superseded s (rows.filter (inScope s)) head = false := by
  have hp := List.find?_some h
  simp only [Bool.and_eq_true] at hp
  simpa using hp.2

theorem selected_owns_execution (s : Scope) (rows : List Row) (head : Row)
    (h : select s rows = some head) : head.inputDisposition = .execution := by
  have hp := List.find?_some h
  simp only [Bool.and_eq_true, beq_iff_eq] at hp
  exact hp.1

theorem selected_is_normal (s : Scope) (rows : List Row) (head : Row)
    (h : select s rows = some head) : head.purpose = .normal := by
  have hm := selected_is_scoped_member s rows head h
  simp only [List.mem_filter, inScope, Bool.and_eq_true, beq_iff_eq] at hm
  exact hm.2.1.1

theorem title_child_has_no_authority (s : Scope) (parent child : Row)
    (h : child.purpose = .titleAudit) : verifiedEdge s parent child = false := by
  simp [verifiedEdge, inScope, h]

theorem verified_child_supersedes_exact_parent (s : Scope) (rows : List Row)
    (child parent : Row) (hm : child ∈ rows) (hv : verifiedEdge s parent child = true) :
    superseded s rows parent = true := by
  simp only [superseded, List.any_eq_true]
  exact ⟨child, hm, hv⟩

theorem arbitrary_parent_link_has_no_authority (s : Scope) (parent child : Row)
    (h : child.goal ≠ some s.goal) : verifiedEdge s parent child = false := by
  simp [verifiedEdge, h]

theorem invalid_receipt_has_no_authority (s : Scope) (parent child : Row)
    (h : child.receiptValid = false) : verifiedEdge s parent child = false := by
  simp [verifiedEdge, h]

theorem wrong_physical_binding_has_no_authority (s : Scope) (parent child : Row)
    (h : child.parentDoc ≠ some parent.doc) : verifiedEdge s parent child = false := by
  simp [verifiedEdge, h]

theorem foreign_child_has_no_authority (s : Scope) (parent child : Row)
    (h : child.owner ≠ s.owner) : verifiedEdge s parent child = false := by
  simp [verifiedEdge, inScope, h]

theorem canonical_order_among_heads (s : Scope) (rows : List Row) (head : Row)
    (h : select s rows = some head) :
    ∃ before after, rows.filter (inScope s) = before ++ head :: after ∧
      ∀ row ∈ before, row.inputDisposition ≠ .execution ∨
        superseded s (rows.filter (inScope s)) row = true := by
  have hf := List.find?_eq_some_iff_append.mp h
  obtain ⟨before, after, heq, hp⟩ := hf.2
  exact ⟨before, after, heq, by simpa using hp⟩

end GoalAutomation.RequestHead
