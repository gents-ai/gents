import Proofs.GoalAutomation

namespace GoalAutomation.Edit

structure Assignment where
  status : Goals.Status
  sequence : Nat
  objective : String
  budget : Option Int
  deriving DecidableEq, Repr

structure Fields where
  objective : Option String
  budget : Option Int
  deriving DecidableEq, Repr

/-- Editing preserves the current assignment identity and continuation epoch.
Only an active observed epoch may change fields; completion, blocked audits,
operator resume and Task reassignment retain their existing owners. A missing
budget preserves its value. Creation claims describe the original admission
and are never rewritten by an edit. -/
def apply? (observed current : Assignment) (fields : Fields) : Option Assignment :=
  if observed.status != .active || current.status != .active ||
      observed.sequence != current.sequence then none
  else if fields.objective.isNone && fields.budget.isNone then none
  else if fields.objective.any (fun objective => (canonicalObjective objective).isEmpty) ||
      !validBudget fields.budget then none
  else some { current with
    objective := fields.objective.map canonicalObjective |>.getD current.objective
    budget := fields.budget.or current.budget }

theorem edit_preserves_status_and_epoch (observed current next : Assignment) (fields : Fields)
    (h : apply? observed current fields = some next) :
    next.status = current.status ∧ next.sequence = current.sequence := by
  unfold apply? at h
  split at h
  · contradiction
  · split at h
    · contradiction
    · split at h
      · contradiction
      · cases h
        exact ⟨rfl, rfl⟩

theorem stale_epoch_cannot_edit (observed current : Assignment) (fields : Fields)
    (h : observed.sequence ≠ current.sequence) : apply? observed current fields = none := by
  simp [apply?, h]

theorem inactive_goal_cannot_edit (observed current : Assignment) (fields : Fields)
    (h : current.status ≠ .active) : apply? observed current fields = none := by
  simp [apply?, h]

end GoalAutomation.Edit
