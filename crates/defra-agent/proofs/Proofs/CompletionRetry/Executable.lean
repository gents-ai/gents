import Proofs.CompletionRetry.Transition
import Mathlib.Tactic.SplitIfs

/-!
# Executable CompletionRetry Semantics

Executable actions, the total step function `step?`, and its equivalence with
the relational `Transition` model (`step_sound` / `transition_complete`).

Every failure action carries the observed `FailureClass` *and* the selected
wake time, so `step?` genuinely consumes both — the classification *and* the
fail-fast decision. On a pre-stream failure the executable semantics dispatches
on the class and, when the retry budget is spent or the selected wake does not
fit the claimed deadline, routes to the matching terminal `exhausted`/`failed`
outcome. Wake-time and delay *values* are chosen by the action's data, not the
model; the model constrains only budget counts and deadline fit.
-/

namespace CompletionRetry

/-- Deadline fit is decidable, so it can gate `step?`'s guards directly. -/
instance decFitsDeadline (wake : Time) (deadline : Option Time) :
    Decidable (fitsDeadline wake deadline) := by
  unfold fitsDeadline
  cases deadline with
  | none => exact isTrue trivial
  | some d => exact inferInstanceAs (Decidable (wake ≤ d))

/-- Executable actions mirroring `Transition`. A single `preStreamFail` action
carries the classification `c`, the (opaque) error text, and the selected wake
time; `step?` dispatches it to the transport/resample/repair/permanent branch
and, on budget/deadline overshoot, to the matching exhaust outcome. The
turn-close continuation `continueAfterClose` likewise fails fast to
`exhausted` when its selected wake overshoots. -/
inductive Action where
  | issue
  | toolEffect
  | streamOk
  | preStreamFail (c : FailureClass) (err : String) (wake : Time)
  | retract (wake : Time)
  | closeTurn
  | continueAfterClose (wake : Time)
  | wake (w : Time)
  | repairIssue
  deriving DecidableEq, Repr

/-- Total executable transition function. Every guard is directly decidable and
every deadline check is `fitsDeadline wake s.deadline` on the action's own wake
— no quantifiers. -/
def step? (s : State) : Action → Option State
  | .issue =>
      if s.phase = Phase.issuing then
        some { s with phase := Phase.streaming }
      else none
  | .toolEffect =>
      if s.phase = Phase.streaming then
        some { s with turn := { s.turn with effects := s.turn.effects + 1 } }
      else none
  | .streamOk =>
      if s.phase = Phase.streaming then
        some { s with phase := Phase.turnDone, turn := { s.turn with rendered := 1 } }
      else none
  | .preStreamFail c err wake =>
      match c with
      | FailureClass.transport =>
          if s.phase = Phase.streaming ∧ s.turn.effects = 0 ∧
             s.transportUsed < s.budget.transportRetries ∧
             fitsDeadline wake s.deadline ∧ s.now ≤ wake then
            some { s with phase := Phase.backingOff wake,
                          transportUsed := s.transportUsed + 1 }
          else if s.phase = Phase.streaming ∧ s.now ≤ wake ∧
                  (s.transportUsed ≥ s.budget.transportRetries ∨
                   ¬ fitsDeadline wake s.deadline) then
            some { s with phase := Phase.exhausted }
          else none
      | FailureClass.parseBadRequest =>
          if s.phase = Phase.streaming ∧ s.turn.effects = 0 ∧
             s.lastParseError ≠ some err ∧
             s.resampleUsed < s.budget.resampleRetries ∧
             fitsDeadline wake s.deadline ∧ s.now ≤ wake then
            some { s with phase := Phase.backingOff wake,
                          resampleUsed := s.resampleUsed + 1,
                          lastParseError := some err }
          else if s.phase = Phase.streaming ∧ s.turn.effects = 0 ∧
                  (s.lastParseError = some err ∨
                   s.resampleUsed ≥ s.budget.resampleRetries) ∧
                  s.budget.allowRepair ∧ ¬ s.repairUsed then
            some { s with phase := Phase.repairing, lastParseError := some err }
          else if s.phase = Phase.streaming ∧ s.now ≤ wake then
            some { s with phase := Phase.exhausted }
          else none
      | FailureClass.permanent =>
          if s.phase = Phase.streaming then
            some { s with phase := Phase.failedPermanent }
          else none
  | .retract wake =>
      if s.phase = Phase.streaming ∧ s.turn.effects = 0 ∧
         s.transportUsed < s.budget.transportRetries ∧
         fitsDeadline wake s.deadline ∧ s.now ≤ wake then
        some { s with phase := Phase.backingOff wake,
                      transportUsed := s.transportUsed + 1,
                      turn := { s.turn with rendered := 0 } }
      else none
  | .closeTurn =>
      if s.phase = Phase.streaming ∧ 0 < s.turn.effects then
        some { s with phase := Phase.turnClosed,
                      turn := { s.turn with rendered := 1 } }
      else none
  | .continueAfterClose wake =>
      if s.phase = Phase.turnClosed ∧
         s.transportUsed < s.budget.transportRetries ∧
         fitsDeadline wake s.deadline ∧ s.now ≤ wake then
        some { s with phase := Phase.backingOff wake,
                      transportUsed := s.transportUsed + 1,
                      turn := { turnIndex := s.turn.turnIndex + 1,
                                effects := 0, rendered := 0 } }
      else if s.phase = Phase.turnClosed ∧ s.now ≤ wake ∧
              (s.transportUsed ≥ s.budget.transportRetries ∨
               ¬ fitsDeadline wake s.deadline) then
        some { s with phase := Phase.exhausted }
      else none
  | .wake w =>
      if s.phase = Phase.backingOff w then
        some { s with phase := Phase.issuing, now := w }
      else none
  | .repairIssue =>
      if s.phase = Phase.repairing ∧ ¬ s.repairUsed then
        some { s with phase := Phase.issuing, repairUsed := true }
      else none

/-- Every executable step is a legal transition. -/
theorem step_sound {s s' : State} {a : Action}
    (h : step? s a = some s') : Transition s s' := by
  cases a with
  | issue =>
    simp only [step?] at h
    split_ifs at h with h1
    simp only [Option.some.injEq] at h; subst h
    exact Transition.issue s h1
  | toolEffect =>
    simp only [step?] at h
    split_ifs at h with h1
    simp only [Option.some.injEq] at h; subst h
    exact Transition.toolEffect s h1
  | streamOk =>
    simp only [step?] at h
    split_ifs at h with h1
    simp only [Option.some.injEq] at h; subst h
    exact Transition.streamOk s h1
  | preStreamFail c err wake =>
    cases c with
    | transport =>
      simp only [step?] at h
      split_ifs at h with h1 h2
      · obtain ⟨hp, heff, hbud, hfit, hfwd⟩ := h1
        simp only [Option.some.injEq] at h; subst h
        exact Transition.transportBackoff s FailureClass.transport wake rfl hp heff hbud hfit hfwd
      · obtain ⟨hp, hfwd, hex⟩ := h2
        simp only [Option.some.injEq] at h; subst h
        exact Transition.transportExhaust s FailureClass.transport wake rfl hp hfwd hex
    | parseBadRequest =>
      simp only [step?] at h
      split_ifs at h with h1 h2 h3
      · obtain ⟨hp, heff, hfresh, hbud, hfit, hfwd⟩ := h1
        simp only [Option.some.injEq] at h; subst h
        exact Transition.resampleBackoff s FailureClass.parseBadRequest err wake rfl hp heff
          hfresh hbud hfit hfwd
      · obtain ⟨hp, heff, hdet, hallow, hunused⟩ := h2
        simp only [Option.some.injEq] at h; subst h
        exact Transition.repair s FailureClass.parseBadRequest err rfl hp heff hdet hallow hunused
      · obtain ⟨hp, hfwd⟩ := h3
        simp only [Option.some.injEq] at h; subst h
        refine Transition.parseExhaust s FailureClass.parseBadRequest err wake rfl hp hfwd ?_ ?_
        · intro hrest; exact h1 ⟨hp, hrest⟩
        · intro hrest; exact h2 ⟨hp, hrest⟩
    | permanent =>
      simp only [step?] at h
      split_ifs at h with h1
      simp only [Option.some.injEq] at h; subst h
      exact Transition.failPermanent s FailureClass.permanent rfl h1
  | retract wake =>
    simp only [step?] at h
    split_ifs at h with h1
    obtain ⟨hp, heff, hbud, hfit, hfwd⟩ := h1
    simp only [Option.some.injEq] at h; subst h
    exact Transition.retract s wake hp heff hbud hfit hfwd
  | closeTurn =>
    simp only [step?] at h
    split_ifs at h with h1
    obtain ⟨hp, heff⟩ := h1
    simp only [Option.some.injEq] at h; subst h
    exact Transition.closeTurn s hp heff
  | continueAfterClose wake =>
    simp only [step?] at h
    split_ifs at h with h1 h2
    · obtain ⟨hp, hbud, hfit, hfwd⟩ := h1
      simp only [Option.some.injEq] at h; subst h
      exact Transition.continueAfterClose s wake hp hbud hfit hfwd
    · obtain ⟨hp, hfwd, hex⟩ := h2
      simp only [Option.some.injEq] at h; subst h
      exact Transition.closeExhaust s wake hp hfwd hex
  | wake w =>
    simp only [step?] at h
    split_ifs at h with h1
    simp only [Option.some.injEq] at h; subst h
    exact Transition.wake s w h1
  | repairIssue =>
    simp only [step?] at h
    split_ifs at h with h1
    obtain ⟨hp, hunused⟩ := h1
    simp only [Option.some.injEq] at h; subst h
    exact Transition.repairIssue s hp hunused

/-- Every legal transition is realized by some executable step. -/
theorem transition_complete {s s' : State} (t : Transition s s') :
    ∃ a, step? s a = some s' := by
  cases t with
  | issue h =>
    exact ⟨.issue, by simp [step?, h]⟩
  | toolEffect h =>
    exact ⟨.toolEffect, by simp [step?, h]⟩
  | streamOk h =>
    exact ⟨.streamOk, by simp [step?, h]⟩
  | transportBackoff c wake hc hp heff hbud hfit hfwd =>
    subst hc
    exact ⟨.preStreamFail FailureClass.transport "" wake,
      by simp [step?, hp, heff, hbud, hfit, hfwd]⟩
  | transportExhaust c wake hc hp hfwd hex =>
    subst hc
    refine ⟨.preStreamFail FailureClass.transport "" wake, ?_⟩
    have hnc1 : ¬ (s.phase = Phase.streaming ∧ s.turn.effects = 0 ∧
        s.transportUsed < s.budget.transportRetries ∧
        fitsDeadline wake s.deadline ∧ s.now ≤ wake) := by
      rintro ⟨-, -, hlt, hfit, -⟩
      rcases hex with hge | hnf
      · omega
      · exact hnf hfit
    simp only [step?]
    rw [if_neg hnc1,
        if_pos (show s.phase = Phase.streaming ∧ s.now ≤ wake ∧
          (s.transportUsed ≥ s.budget.transportRetries ∨ ¬ fitsDeadline wake s.deadline)
          from ⟨hp, hfwd, hex⟩)]
  | resampleBackoff c err wake hc hp heff hfresh hbud hfit hfwd =>
    subst hc
    exact ⟨.preStreamFail FailureClass.parseBadRequest err wake,
      by simp [step?, hp, heff, hfresh, hbud, hfit, hfwd]⟩
  | repair c err hc hp heff hdet hallow hunused =>
    subst hc
    refine ⟨.preStreamFail FailureClass.parseBadRequest err 0, ?_⟩
    have hnc1 : ¬ (s.phase = Phase.streaming ∧ s.turn.effects = 0 ∧
        s.lastParseError ≠ some err ∧ s.resampleUsed < s.budget.resampleRetries ∧
        fitsDeadline 0 s.deadline ∧ s.now ≤ 0) := by
      rintro ⟨-, -, hne, hlt, -, -⟩
      rcases hdet with he | hge
      · exact hne he
      · omega
    simp only [step?]
    rw [if_neg hnc1,
        if_pos (show s.phase = Phase.streaming ∧ s.turn.effects = 0 ∧
          (s.lastParseError = some err ∨ s.resampleUsed ≥ s.budget.resampleRetries) ∧
          s.budget.allowRepair ∧ ¬ s.repairUsed
          from ⟨hp, heff, hdet, hallow, hunused⟩)]
  | parseExhaust c err wake hc hp hfwd hno_resample hno_repair =>
    subst hc
    refine ⟨.preStreamFail FailureClass.parseBadRequest err wake, ?_⟩
    have hnc1 : ¬ (s.phase = Phase.streaming ∧ s.turn.effects = 0 ∧
        s.lastParseError ≠ some err ∧ s.resampleUsed < s.budget.resampleRetries ∧
        fitsDeadline wake s.deadline ∧ s.now ≤ wake) := by
      rintro ⟨-, hrest⟩; exact hno_resample hrest
    have hnc2 : ¬ (s.phase = Phase.streaming ∧ s.turn.effects = 0 ∧
        (s.lastParseError = some err ∨ s.resampleUsed ≥ s.budget.resampleRetries) ∧
        s.budget.allowRepair ∧ ¬ s.repairUsed) := by
      rintro ⟨-, hrest⟩; exact hno_repair hrest
    simp only [step?]
    rw [if_neg hnc1, if_neg hnc2,
        if_pos (show s.phase = Phase.streaming ∧ s.now ≤ wake from ⟨hp, hfwd⟩)]
  | failPermanent c hc hp =>
    subst hc
    exact ⟨.preStreamFail FailureClass.permanent "" 0, by simp [step?, hp]⟩
  | retract wake hp heff hbud hfit hfwd =>
    exact ⟨.retract wake, by simp [step?, hp, heff, hbud, hfit, hfwd]⟩
  | closeTurn hp heff =>
    exact ⟨.closeTurn, by simp [step?, hp, heff]⟩
  | continueAfterClose wake hp hbud hfit hfwd =>
    exact ⟨.continueAfterClose wake, by simp [step?, hp, hbud, hfit, hfwd]⟩
  | closeExhaust wake hp hfwd hex =>
    refine ⟨.continueAfterClose wake, ?_⟩
    have hnc1 : ¬ (s.phase = Phase.turnClosed ∧
        s.transportUsed < s.budget.transportRetries ∧
        fitsDeadline wake s.deadline ∧ s.now ≤ wake) := by
      rintro ⟨-, hlt, hfit, -⟩
      rcases hex with hge | hnf
      · omega
      · exact hnf hfit
    simp only [step?]
    rw [if_neg hnc1,
        if_pos (show s.phase = Phase.turnClosed ∧ s.now ≤ wake ∧
          (s.transportUsed ≥ s.budget.transportRetries ∨ ¬ fitsDeadline wake s.deadline)
          from ⟨hp, hfwd, hex⟩)]
  | wake w hp =>
    exact ⟨.wake w, by simp [step?, hp]⟩
  | repairIssue hp hunused =>
    exact ⟨.repairIssue, by simp [step?, hp, hunused]⟩

end CompletionRetry
