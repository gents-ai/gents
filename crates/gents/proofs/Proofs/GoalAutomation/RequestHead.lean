import Proofs.GoalAutomation

/-! Candidate-local refinement: the selector changes ordering only when a
verified current-Goal child proves its exact physical parent superseded.
Invalid/missing edges have no ordering authority. Selecting a row does NOT
admit it or authorize publication; existing owner checks still apply.
-/
namespace GoalAutomation.RequestHead

structure Scope where
  owner : Nat
  session : Nat
  goal : Nat
  deriving DecidableEq, Repr

structure Row where
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
  deriving DecidableEq, Repr

def inScope (s : Scope) (r : Row) : Bool := r.owner == s.owner && r.session == s.session

/-- Cheap qualification/binding checks precede cryptographic validation.
The shared Goal physical-edge helper checks original parent/child signatures,
owner/session, exact physical parent pair, canonical Goal metadata and original
deterministic request/retry identity. Historical omission of inherited source
fields does not erase a signed physical edge. Fresh typed-resume receipt checks
remain stricter; this ordering projection does not authorize publication. -/
def verifiedEdge (s : Scope) (parent child : Row) : Bool :=
  child.goal == some s.goal && child.parentDoc == some parent.doc &&
  child.parentRequest == some parent.request && child.doc != parent.doc &&
  inScope s parent && inScope s child && child.sequence > 0 &&
  child.receiptValid && child.deterministicIdentity

def superseded (s : Scope) (rows : List Row) (parent : Row) : Bool :=
  rows.any (verifiedEdge s parent)

/-- Input order is the existing canonical query order. Multiple unrelated or
branched leaves retain that order. No-head (empty or all superseded) cannot
authorize a new continuation. Busy/unknown-state guards remain outside here. -/
def select (s : Scope) (orderedRows : List Row) : Option Row :=
  let rows := orderedRows.filter (inScope s)
  rows.find? fun row => !superseded s rows row

theorem selected_is_scoped_member (s : Scope) (rows : List Row) (head : Row)
    (h : select s rows = some head) : head ∈ rows.filter (inScope s) :=
  List.mem_of_find?_eq_some h

theorem selected_has_no_verified_child (s : Scope) (rows : List Row) (head : Row)
    (h : select s rows = some head) : superseded s (rows.filter (inScope s)) head = false := by
  have hp := List.find?_some h
  simpa using hp

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
      ∀ row ∈ before, superseded s (rows.filter (inScope s)) row = true := by
  have hf := List.find?_eq_some_iff_append.mp h
  obtain ⟨before, after, heq, hp⟩ := hf.2
  exact ⟨before, after, heq, by simpa using hp⟩

end GoalAutomation.RequestHead
