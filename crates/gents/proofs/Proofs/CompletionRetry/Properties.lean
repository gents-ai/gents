import Proofs.CompletionRetry.Transition

namespace CompletionRetry

def ReissueInv (s : State) : Prop :=
  (s.phase = Phase.issuing ∨ (∃ w, s.phase = Phase.backingOff w) ∨
   s.phase = Phase.repairing) → s.turn.effects = 0

theorem reissue_inv_preserved
    {s s' : State} (t : Transition s s') (h : ReissueInv s) :
    ReissueInv s' := by
  cases t <;> simp_all [ReissueInv]

theorem n1_reissue_requires_no_open_effects
    {s s' : State} (_t : Transition s s')
    (hinv : ReissueInv s) (h : s.phase = Phase.issuing) :
    s.turn.effects = 0 := by
  exact hinv (Or.inl h)

theorem n2_retract_only_before_effects
    {s s' : State} (t : Transition s s')
    (hbound : s.turn.rendered ≤ 1)
    (hr : s'.turn.rendered < s.turn.rendered)
    (hsame : s'.turn.turnIndex = s.turn.turnIndex) :
    s.turn.effects = 0 := by
  cases t <;> simp_all <;> omega

theorem n3_budget_monotone_bounded
    {s s' : State} (t : Transition s s')
    (hb : s.transportUsed ≤ s.budget.transportRetries ∧
          s.resampleUsed ≤ s.budget.resampleRetries) :
    s.transportUsed ≤ s'.transportUsed ∧
    s.resampleUsed ≤ s'.resampleUsed ∧
    s'.transportUsed ≤ s'.budget.transportRetries ∧
    s'.resampleUsed ≤ s'.budget.resampleRetries := by
  cases t <;> simp_all <;> omega

theorem n3_repair_at_most_once
    {s s' : State} (t : Transition s s') (h : s.repairUsed = true) :
    s'.repairUsed = true := by
  cases t <;> simp_all

theorem n4_backoff_fits_deadline
    {s s' : State} {w : Time} (t : Transition s s')
    (h : s'.phase = Phase.backingOff w) :
    fitsDeadline w s.deadline ∧ s.now ≤ w ∧ s'.deadline = s.deadline := by
  cases t <;> simp_all

theorem n5_rendered_at_most_one
    {s s' : State} (t : Transition s s') (h : s.turn.rendered ≤ 1) :
    s'.turn.rendered ≤ 1 := by
  cases t <;> simp_all

end CompletionRetry
