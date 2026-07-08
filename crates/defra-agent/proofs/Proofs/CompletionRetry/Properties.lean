import Proofs.CompletionRetry.Transition

/-!
# CompletionRetry Properties (N1–N5)

The safety obligations of per-completion retry: no tool re-execution on retry,
retraction only before effects, bounded budgets with at-most-once repair,
backoff that fits the claimed deadline, and at most one retained rendered turn.
-/

namespace CompletionRetry

/-- Re-issue invariant: away from an active stream (and outside a closed
turn awaiting continuation), the current turn has no open effects. This is
the inductive carrier for N1 — `continueAfterClose` re-establishes it by
resetting the (new) turn's counters. -/
def ReissueInv (s : State) : Prop :=
  (s.phase = Phase.issuing ∨ (∃ w, s.phase = Phase.backingOff w) ∨
   s.phase = Phase.repairing) → s.turn.effects = 0

theorem reissue_inv_preserved
    {s s' : State} (t : Transition s s') (h : ReissueInv s) :
    ReissueInv s' := by
  cases t <;> simp_all [ReissueInv]

/-- N1: from any invariant-satisfying state, issuing a completion
(`issuing → streaming`) happens with zero open effects — a retried or
repaired completion can never face un-accounted tool executions, so retry
never re-executes tools. -/
theorem n1_reissue_requires_no_open_effects
    {s s' : State} (_t : Transition s s')
    (hinv : ReissueInv s) (h : s.phase = Phase.issuing) :
    s.turn.effects = 0 := by
  exact hinv (Or.inl h)

/-- N2: a same-turn rendered decrease (retraction) requires zero effects
this turn. `continueAfterClose` increments the turn index, so its counter
reset is starting a new turn, not retracting the closed one. The bound
`s.turn.rendered ≤ 1` is the N5 invariant carried in: without it, `streamOk`
and `closeTurn` (which set `rendered := 1`) would look like decreases from an
unreachable `rendered ≥ 2` even though they permit open effects. -/
theorem n2_retract_only_before_effects
    {s s' : State} (t : Transition s s')
    (hbound : s.turn.rendered ≤ 1)
    (hr : s'.turn.rendered < s.turn.rendered)
    (hsame : s'.turn.turnIndex = s.turn.turnIndex) :
    s.turn.effects = 0 := by
  cases t <;> simp_all <;> omega

/-- N3a: budget counters never decrease and never exceed their budgets. -/
theorem n3_budget_monotone_bounded
    {s s' : State} (t : Transition s s')
    (hb : s.transportUsed ≤ s.budget.transportRetries ∧
          s.resampleUsed ≤ s.budget.resampleRetries) :
    s.transportUsed ≤ s'.transportUsed ∧
    s.resampleUsed ≤ s'.resampleUsed ∧
    s'.transportUsed ≤ s'.budget.transportRetries ∧
    s'.resampleUsed ≤ s'.budget.resampleRetries := by
  cases t <;> simp_all <;> omega

/-- N3b: repair happens at most once — `repairUsed` is monotone and the
`repair` transition requires it unset. -/
theorem n3_repair_at_most_once
    {s s' : State} (t : Transition s s') (h : s.repairUsed = true) :
    s'.repairUsed = true := by
  cases t <;> simp_all

/-- N4: every backoff wake time fits the deadline and never moves the
clock backwards; retry cannot extend the deadline (deadline is immutable
across all transitions). -/
theorem n4_backoff_fits_deadline
    {s s' : State} {w : Time} (t : Transition s s')
    (h : s'.phase = Phase.backingOff w) :
    fitsDeadline w s.deadline ∧ s.now ≤ w ∧ s'.deadline = s.deadline := by
  cases t <;> simp_all

/-- N5: the current turn retains at most one rendered instance. -/
theorem n5_rendered_at_most_one
    {s s' : State} (t : Transition s s') (h : s.turn.rendered ≤ 1) :
    s'.turn.rendered ≤ 1 := by
  cases t <;> simp_all

end CompletionRetry
