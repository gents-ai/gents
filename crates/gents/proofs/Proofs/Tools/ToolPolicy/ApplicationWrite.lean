import Mathlib.Tactic.Tauto
namespace ToolPolicy.ApplicationWrite
inductive Operation where
  | create | update | delete
  deriving BEq, Repr
structure Observation where
  granted : Bool
  application : Bool
  blocked : Bool
  credential : Bool
  bounded : Bool
  targets : Nat
  limit : Nat
  preview : Bool
  digestMatches : Bool
  deriving Repr
/-- Owner models admission and observed-state preview only; DefraDB validates
fields and enforces DID/ACP within the transaction, and product documents,
credentials and protected evaluation material retain their owners. -/
def admitted (op : Operation) (o : Observation) : Bool :=
  o.granted && o.application && !o.blocked && !o.credential && o.bounded &&
  (o.limit > 0 && o.limit ≤ 100) && o.targets ≤ o.limit && (op == .create || o.targets > 0)
def mayApply (op : Operation) (o : Observation) : Bool :=
  admitted op o && !o.preview && o.digestMatches
theorem apply_requires_grant (op : Operation) (o : Observation) :
    mayApply op o = true → o.granted = true := by
  simp only [mayApply, admitted, Bool.and_eq_true]; tauto
theorem apply_requires_observed_digest (op : Operation) (o : Observation) :
    mayApply op o = true → o.digestMatches = true := by
  simp only [mayApply, Bool.and_eq_true]; tauto
theorem preview_never_applies (op : Operation) (o : Observation) (h : o.preview = true) :
    mayApply op o = false := by simp [mayApply, h]

/-! ## What a preview's digest binds

A digest binds the call (operation, collection, input, filter, bound), the
collection's schema and the documents the preview depends on. An update or
delete depends on its targets: the documents its filter selected, each with
its version, since its effect is to write exactly them. A create depends on no
document: its effect is one new document holding its input, which no other
document's state changes. Several creates previewed in one turn therefore
apply in any order, while an update or delete is re-previewed when what it
selected changed.

The digest is a staleness check between a preview and its apply, read and
compared inside the apply's transaction; it is not a commit fence. What fences
the apply at commit (`TransactionFence`): an update or delete writes each
target by document id, so a concurrent writer of a target aborts one of the
two (`commonWrite`) and the rebuilt apply finds a stale digest; the target
read is filtered and not validated at commit, so a document that joins the
filter while the apply is open is neither written nor reported, and the effect
stays the previewed targets. A create's id is derived from its content and
signer, so a concurrent identical create writes the same document
(`commonWrite`), and a unique index conflicts at commit (`uniqueIndex`). -/

/-- A document of the pending preview's collection: its id, its version, and
whether the pending filter selects it. -/
structure Doc where
  id : Nat
  version : Nat
  selected : Bool
  deriving DecidableEq, Repr

/-- A write applied between a pending preview and its apply, in the pending
preview's collection. -/
inductive Sibling where
  /-- Creates a document the pending filter does not select. -/
  | createOutside
  /-- Creates a document the pending filter selects. -/
  | createInside
  /-- Changes a document the pending preview selected. -/
  | updateTarget
  /-- Changes a document the pending preview did not select, which stays
  unselected. -/
  | updateOther
  /-- Deletes a document the pending preview selected. -/
  | deleteTarget
  deriving DecidableEq, Repr

def bindsTargets : Operation → Bool
  | .create => false
  | _ => true

/-- The documents a digest binds, with their versions. -/
def bound (op : Operation) (docs : List Doc) : List (Nat × Nat) :=
  if bindsTargets op then (docs.filter (·.selected)).map fun d => (d.id, d.version) else []

def bump (selected : Bool) : List Doc → List Doc
  | [] => []
  | d :: rest =>
    if d.selected == selected then { d with version := d.version + 1 } :: rest
    else d :: bump selected rest

def removeFirstSelected : List Doc → List Doc
  | [] => []
  | d :: rest => if d.selected then rest else d :: removeFirstSelected rest

def fresh (docs : List Doc) : Nat := (docs.map (·.id)).foldl max 0 + 1

def applySibling (docs : List Doc) : Sibling → List Doc
  | .createOutside => docs ++ [⟨fresh docs, 0, false⟩]
  | .createInside => docs ++ [⟨fresh docs, 0, true⟩]
  | .updateTarget => bump true docs
  | .updateOther => bump false docs
  | .deleteTarget => removeFirstSelected docs

/-- The collection the cases observe: one document the pending filter
selects and one it does not. -/
def observed : List Doc := [⟨1, 0, true⟩, ⟨2, 0, false⟩]

/-- Whether applying `sibling` first makes the pending preview's digest stale. -/
def staleAfter (operation : Operation) (sibling : Sibling) : Bool :=
  decide (bound operation observed ≠ bound operation (applySibling observed sibling))

/-- No sibling write stales a create's preview: sibling creates apply in any
order. -/
theorem a_create_preview_survives_every_sibling (docs : List Doc) (sibling : Sibling) :
    bound .create docs = bound .create (applySibling docs sibling) := by
  simp [bound, bindsTargets]

theorem a_change_outside_the_targets_keeps_an_update_or_delete
    (operation : Operation) (h : operation ≠ .create) :
    staleAfter operation .createOutside = false ∧ staleAfter operation .updateOther = false := by
  cases operation <;> simp_all [staleAfter, bound, bindsTargets, applySibling, observed, bump,
    fresh]

theorem a_change_to_the_targets_stales_an_update_or_delete
    (operation : Operation) (h : operation ≠ .create) :
    staleAfter operation .createInside = true ∧ staleAfter operation .updateTarget = true ∧
      staleAfter operation .deleteTarget = true := by
  cases operation <;> simp_all [staleAfter, bound, bindsTargets, applySibling, observed, bump,
    removeFirstSelected, fresh]

end ToolPolicy.ApplicationWrite
