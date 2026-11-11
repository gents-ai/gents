import Mathlib.Data.List.Basic
import Lean.Elab.Tactic.Omega

namespace Background.Observation

/-- Native subscriptions are live-only hints. A monotonic clock, eventual
polling of ready timers, and termination of each durable read are external
premises; this owner proves wake selection, not host scheduling fairness.
Only the timer owner advances `rescanAt`, independently of change traffic. -/
structure State where
  rescanAt : Nat
  feedOpen : Bool
  deriving DecidableEq, Repr

inductive Change where
  | quiet
  | unrelated
  | relevant
  | overflow
  | closed
  deriving DecidableEq, Repr

structure Input where
  now : Nat
  cancelled : Bool
  deadlineAt : Nat
  change : Change
  deriving DecidableEq, Repr

inductive Wake where
  | cancel
  | deadline
  | rescan
  | changed
  | wait
  deriving DecidableEq, Repr

def wake (state : State) (input : Input) : Wake :=
  if input.cancelled then .cancel
  else if input.deadlineAt ≤ input.now then .deadline
  else if state.rescanAt ≤ input.now then .rescan
  else if state.feedOpen && input.change != .quiet && input.change != .unrelated
    then .changed else .wait

def observeChange (state : State) (change : Change) : State :=
  { state with feedOpen := state.feedOpen && change != .closed }

theorem cancellation_precedes_all_wakes (state : State) (input : Input)
    (cancelled : input.cancelled = true) : wake state input = .cancel := by
  simp [wake, cancelled]

theorem due_deadline_precedes_change (state : State) (input : Input)
    (live : input.cancelled = false) (due : input.deadlineAt ≤ input.now) :
    wake state input = .deadline := by simp [wake, live, due]

theorem due_rescan_cannot_be_starved (state : State) (input : Input)
    (live : input.cancelled = false) (before : input.now < input.deadlineAt)
    (due : state.rescanAt ≤ input.now) : wake state input = .rescan := by
  simp [wake, live, Nat.not_le.mpr before, due]

theorem notification_preserves_rescan (state : State) (change : Change) :
    (observeChange state change).rescanAt = state.rescanAt := by rfl

theorem arbitrary_traffic_preserves_rescan (state : State) (changes : List Change) :
    (changes.foldl observeChange state).rescanAt = state.rescanAt := by
  induction changes generalizing state with
  | nil => rfl
  | cons change rest ih => simpa [List.foldl] using ih (observeChange state change)

theorem feed_closure_is_latched (state : State) (changes : List Change) :
    (changes.foldl observeChange (observeChange state .closed)).feedOpen = false := by
  have closed (s : State) (xs : List Change) (h : s.feedOpen = false) :
      (xs.foldl observeChange s).feedOpen = false := by
    induction xs generalizing s with
    | nil => simpa using h
    | cons x xs ih =>
      apply ih
      simp [observeChange, h]
  apply closed
  simp [observeChange]

theorem closed_feed_keeps_durable_recovery (state : State) (input : Input)
    (live : input.cancelled = false) (before : input.now < input.deadlineAt)
    (due : state.rescanAt ≤ input.now) :
    wake (observeChange state .closed) input = .rescan := by
  exact due_rescan_cannot_be_starved (observeChange state .closed) input live before due

end Background.Observation
