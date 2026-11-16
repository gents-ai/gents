import Proofs.Session.Hooks.Staging
import Proofs.Session.Hooks.OwedDelivery
import Proofs.Automation.Goals

namespace Hooks.GoalHalt

/-- An instant on the timeline, as the listener reads an RFC 3339 time: the
text is parsed and the instants are compared, never the text. Text order is
not time order across offsets ("10:00+02:00" is before "09:00Z") or across
fractional-second precision. -/
abbrev Instant := Int

/-- The session's canonical Goal as the listener is shown it: its state and
its last change time (`updated_at`), `none` when that is missing or not RFC
3339. -/
structure Goal where
  state : Goals.State
  updatedAt : Option Instant
  deriving DecidableEq, Repr

/-- `control.session.halt`: the halted session and the halt's time, `none`
when it is not RFC 3339. A live halt carries the time of its own transaction;
a redriven or re-derived one carries the time it was latched
(`Hooks.OwedDelivery.deriveHalts`). -/
structure Halt where
  session : String
  time : Option Instant
  deriving DecidableEq, Repr

inductive Query where
  | goal (session : String)
  deriving DecidableEq, Repr

def reads (halt : Halt) : List Query := [.goal halt.session]

/-- The Goal changed after the halt: its last change time is strictly later
than the halt's.

Decision 2026-11-11, maintainer-approved: a halt pauses the session's Goal
unless the Goal row shows an explicit change after the halt. On equal times
the Goal is paused: the halt is the user's request that work stop, and only a
change it cannot have seen may outrank it. A missing or unparseable change
time is not evidence of a later change, so the Goal is paused. -/
def changedAfter (goal : Goal) (halt : Instant) : Bool :=
  goal.updatedAt.any (halt < ·)

/-- What the listener decides for a halt. -/
inductive Verdict where
  | inactive
  | changedAfter
  | unreadableHalt
  | pause (paused : Goals.State)
  deriving DecidableEq, Repr

/-- `goal::lifecycle_hooks::halt_verdict`: no Goal, or one a pause does not
apply to, is left alone; otherwise a halt whose time cannot be read is
refused; a Goal changed after the halt is left alone; and any other Goal is
paused.

Why a later change wins: an owed halt is redriven, and the recovery sweep
re-derives a current halt from its latch, after the transition that owed it.
In between, a resume or another explicit change may have moved the Goal. A
pause staged then would overwrite that later change with the decision of an
older halt. A live halt is unaffected: its time is that of its own
transaction, so no committed change is later than it (premise: the runtime's
clock does not run backwards between transactions). -/
def verdict (goal : Option Goal) (time : Option Instant) : Verdict :=
  match goal with
  | none => .inactive
  | some goal =>
      match Goals.step? goal.state .pause with
      | none => .inactive
      | some paused =>
          match time with
          | none => .unreadableHalt
          | some time => if changedAfter goal time then .changedAfter else .pause paused

def settle : String := "trigger.goal_outcomes.settle"

def unreadable : String := "halt time is not RFC 3339"

/-- The Goals listener at `control.session.halt` (`PauseOnHalt`). The first
row shown is the session's canonical Goal. A pause writes the paused state
and the halt's time as the change time, and settles the Goal's fire
outcomes. -/
def decide (halt : Halt) : List (List (Row String Goal)) → Outcome Unit String Goal String
  | [[]] => .proceed () []
  | [row :: _] =>
      match verdict (some row.value) halt.time with
      | .inactive | .changedAfter => .proceed () []
      | .unreadableHalt => .refuse unreadable
      | .pause paused =>
          .proceed () [.write (.update row.ref row.revision ⟨paused, halt.time⟩), .core settle]
  | _ => .refuse "Goal pause was not shown its declared rows"

def plugin : Plugin Query Halt Unit String Goal String :=
  { manifest := { name := "goals", owns := ["Goal"], bound := 16 }
  , hook := { reads := reads, decide := decide } }

/-- A later explicit change to the Goal is not overwritten by a redriven or
re-derived pause. -/
theorem later_change_is_not_overwritten (halt : Halt) (row : Row String Goal)
    (rest : List (Row String Goal)) (time changed : Instant) (halted : halt.time = some time)
    (later : row.value.updatedAt = some changed) (after : time < changed) :
    (decide halt [row :: rest]).effects = [] := by
  simp only [decide, verdict]
  cases Goals.step? row.value.state .pause with
  | none => rfl
  | some paused =>
    simp [halted, changedAfter, later, after, Outcome.effects]

/-- A live halt of an active Goal with no later change pauses it, at the halt's
time; equal times and an unreadable change time included. -/
theorem live_halt_pauses_active_goal (halt : Halt) (row : Row String Goal)
    (rest : List (Row String Goal)) (time : Instant) (halted : halt.time = some time)
    (active : row.value.state.status = .active)
    (unchanged : row.value.updatedAt.all (· ≤ time) = true) :
    decide halt [row :: rest] =
      .proceed () [.write (.update row.ref row.revision
        ⟨{ row.value.state with status := .paused }, some time⟩), .core settle] := by
  have notLater : changedAfter row.value time = false := by
    cases updated : row.value.updatedAt with
    | none => simp [changedAfter, updated]
    | some changed =>
      simp [updated] at unchanged
      simp [changedAfter, updated]
      exact unchanged
  simp [decide, verdict, Goals.step?, active, halted, notLater]

private theorem paused_does_not_pause (state paused : Goals.State)
    (step : Goals.step? state .pause = some paused) : Goals.step? paused .pause = none := by
  simp only [Goals.step?] at step
  split at step
  · cases step
    simp [Goals.step?]
  · cases step

/-- A redrive of a halt that was already applied is inert: the Goal it paused
is no longer pausable. -/
theorem redrive_of_applied_halt_is_inert (halt : Halt) (row : Row String Goal) (paused : Goals.State)
    (applied : verdict (some row.value) halt.time = .pause paused)
    (again : Row String Goal) (written : again.value = ⟨paused, halt.time⟩)
    (later : List (Row String Goal)) :
    (decide halt [again :: later]).effects = [] := by
  have step : Goals.step? row.value.state .pause = some paused := by
    cases stepped : Goals.step? row.value.state .pause with
    | none => simp [verdict, stepped] at applied
    | some next =>
      cases timed : halt.time with
      | none => simp [verdict, stepped, timed] at applied
      | some time =>
        by_cases changed : changedAfter row.value time = true
        · simp [verdict, stepped, timed, changed] at applied
        · simp [verdict, stepped, timed, changed] at applied
          rw [applied]
  simp [decide, verdict, written, paused_does_not_pause _ _ step, Outcome.effects]

/-- A halt the recovery sweep derives carries the latch time of a session it
was derived from. -/
theorem derived_halt_time_is_latch (bound : Nat) (sessions : List OwedDelivery.HaltedSession)
    (session time owner : String)
    (derived : (session, time, owner) ∈ OwedDelivery.deriveHalts bound sessions) :
    ∃ halted ∈ sessions, halted.session = session ∧ halted.latchedAt = some time := by
  unfold OwedDelivery.deriveHalts at derived
  obtain ⟨candidate, member, mapped⟩ := List.mem_filterMap.mp derived
  have present := (List.mem_filter.mp (List.mem_of_mem_take member)).1
  cases owned : candidate.owner <;> cases latched : candidate.latchedAt <;>
    simp [owned, latched] at mapped
  exact ⟨candidate, present, mapped.1, by rw [latched, mapped.2.1]⟩

/-- A re-derived halt, at its latch time, does not overwrite a Goal changed
after the latch. -/
theorem derived_halt_keeps_a_later_change (bound : Nat)
    (sessions : List OwedDelivery.HaltedSession) (session latched owner : String)
    (derived : (session, latched, owner) ∈ OwedDelivery.deriveHalts bound sessions)
    (parse : String → Option Instant) (time changed : Instant) (parsed : parse latched = some time)
    (row : Row String Goal) (rest : List (Row String Goal))
    (later : row.value.updatedAt = some changed) (after : time < changed) :
    (∃ halted ∈ sessions, halted.latchedAt = some latched) ∧
      (decide ⟨session, parse latched⟩ [row :: rest]).effects = [] := by
  obtain ⟨halted, member, _, latch⟩ := derived_halt_time_is_latch bound sessions session latched
    owner derived
  exact ⟨⟨halted, member, latch⟩,
    later_change_is_not_overwritten ⟨session, parse latched⟩ row rest time changed parsed later
      after⟩

end Hooks.GoalHalt
