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

/-- A running agent cannot rewrite paused intent. The signed client-control
boundary may edit a paused goal without resuming its work. -/
inductive Origin where
  | agent | client
  deriving DecidableEq, Repr

def editable (origin : Origin) (status : Goals.Status) : Bool :=
  status == .active || (origin == .client && status == .paused)

/-- Editing preserves the current assignment identity and continuation epoch.
Only a matching editable observed epoch may change fields; completion, blocked
audits, operator resume and Task reassignment retain their existing owners. A missing
budget preserves its value. Creation claims describe the original admission
and are never rewritten by an edit. -/
def apply? (observed current : Assignment) (fields : Fields)
    (origin : Origin := .agent) : Option Assignment :=
  if !editable origin current.status || observed.status != current.status ||
      observed.sequence != current.sequence then none
  else if fields.objective.isNone && fields.budget.isNone then none
  else if fields.objective.any (fun objective => (canonicalObjective objective).isEmpty) ||
      !validBudget fields.budget then none
  else some { current with
    objective := fields.objective.map canonicalObjective |>.getD current.objective
    budget := fields.budget.or current.budget }

theorem edit_preserves_status_and_epoch (observed current next : Assignment) (fields : Fields)
    (origin : Origin) (h : apply? observed current fields origin = some next) :
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
    (origin : Origin) (h : observed.sequence ≠ current.sequence) :
    apply? observed current fields origin = none := by
  simp [apply?, h]

theorem stale_status_cannot_edit (observed current : Assignment) (fields : Fields)
    (origin : Origin) (h : observed.status ≠ current.status) :
    apply? observed current fields origin = none := by
  simp [apply?, h]

theorem uneditable_goal_cannot_edit (observed current : Assignment) (fields : Fields)
    (origin : Origin) (h : editable origin current.status = false) :
    apply? observed current fields origin = none := by
  simp [apply?, h]

theorem agent_cannot_edit_paused_goal (observed current : Assignment) (fields : Fields)
    (h : current.status = .paused) : apply? observed current fields .agent = none := by
  simp [apply?, editable, h]

theorem client_edit_preserves_pause (observed current next : Assignment) (fields : Fields)
    (paused : current.status = .paused)
    (h : apply? observed current fields .client = some next) : next.status = .paused := by
  exact (edit_preserves_status_and_epoch observed current next fields .client h).1.trans paused

end GoalAutomation.Edit
