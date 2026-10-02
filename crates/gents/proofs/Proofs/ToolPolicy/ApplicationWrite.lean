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
/-- Product documents, credentials and protected evaluation material retain their
owners. DefraDB validates fields and enforces DID/ACP within the transaction;
this owner models admission and the observed-state preview, not authorization. -/
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
end ToolPolicy.ApplicationWrite
