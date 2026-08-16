namespace CompletionRetry.OutputObligation

inductive Scope
  | request
  | trigger
  deriving DecidableEq, Repr

structure ActivationContext where
  executionScheduled : Bool
  hasAutomatedTriggerLineage : Bool
  deriving DecidableEq, Repr

def active (scope : Scope) (context : ActivationContext) : Bool :=
  match scope with
  | .request => true
  | .trigger => context.hasAutomatedTriggerLineage

structure State where
  minimumWrites : Nat
  completedWrites : Nat
  expectedWrites : Option Nat := none
  countValid : Bool := true
  deriving DecidableEq, Repr

inductive Decision
  | continue
  | complete
  | reject
  deriving DecidableEq, Repr

def satisfied (state : State) : Prop :=
  state.countValid = true ∧
    state.minimumWrites ≤ state.completedWrites ∧
    match state.expectedWrites with
    | none => True
    | some expected => state.completedWrites = expected

def decideTerminal (state : State) : Decision :=
  if !state.countValid then
    .reject
  else
    match state.expectedWrites with
    | some expected =>
        if expected < state.minimumWrites ∨ expected < state.completedWrites then
          .reject
        else if state.completedWrites = expected then
          .complete
        else
          .continue
    | none =>
        if state.minimumWrites ≤ state.completedWrites then .complete else .continue

def recordWrite (state : State) : State :=
  { state with completedWrites := state.completedWrites + 1 }

theorem unsatisfied_cannot_complete
    (state : State)
    (hvalid : state.countValid = true)
    (hexpected : state.expectedWrites = none)
    (h : state.completedWrites < state.minimumWrites) :
    decideTerminal state = .continue := by
  simp [decideTerminal, hvalid, hexpected, Nat.not_le.mpr h]

theorem satisfied_can_complete
    (state : State)
    (hvalid : state.countValid = true)
    (hminimum : state.minimumWrites ≤ state.completedWrites)
    (hexpected : state.expectedWrites = none) :
    decideTerminal state = .complete := by
  simp [decideTerminal, hvalid, hminimum, hexpected]

theorem writes_are_monotone (state : State) :
    state.completedWrites ≤ (recordWrite state).completedWrites := by
  simp [recordWrite]

theorem enough_writes_eventually_complete (minimumWrites : Nat) :
    decideTerminal { minimumWrites, completedWrites := minimumWrites } = .complete := by
  simp [decideTerminal]

theorem dynamic_incomplete_continues
    (minimumWrites completedWrites expectedWrites : Nat)
    (hminimum : minimumWrites ≤ completedWrites)
    (hincomplete : completedWrites < expectedWrites) :
    decideTerminal
      { minimumWrites
      , completedWrites
      , expectedWrites := some expectedWrites
      , countValid := true
      } = .continue := by
  simp [decideTerminal, Nat.not_lt.mpr (Nat.le_trans hminimum (Nat.le_of_lt hincomplete)),
    Nat.not_lt.mpr (Nat.le_of_lt hincomplete), Nat.ne_of_lt hincomplete]

theorem dynamic_complete_closes
    (minimumWrites expectedWrites : Nat)
    (hminimum : minimumWrites ≤ expectedWrites) :
    decideTerminal
      { minimumWrites
      , completedWrites := expectedWrites
      , expectedWrites := some expectedWrites
      , countValid := true
      } = .complete := by
  simp [decideTerminal, Nat.not_lt.mpr hminimum]

theorem dynamic_overfull_rejects
    (minimumWrites completedWrites expectedWrites : Nat)
    (hoverfull : expectedWrites < completedWrites) :
    decideTerminal
      { minimumWrites
      , completedWrites
      , expectedWrites := some expectedWrites
      , countValid := true
      } = .reject := by
  simp [decideTerminal, hoverfull]

theorem inconsistent_count_rejects (state : State) :
    decideTerminal { state with countValid := false } = .reject := by
  simp [decideTerminal]

theorem trigger_obligation_inactive_for_interactive :
    active .trigger
      { executionScheduled := false, hasAutomatedTriggerLineage := false } = false := by
  rfl

theorem request_obligation_active_for_interactive :
    active .request
      { executionScheduled := false, hasAutomatedTriggerLineage := false } = true := by
  rfl

theorem trigger_obligation_inactive_for_scheduled_control :
    active .trigger
      { executionScheduled := true, hasAutomatedTriggerLineage := false } = false := by
  rfl

theorem trigger_obligation_active_for_automated_trigger :
    active .trigger
      { executionScheduled := true, hasAutomatedTriggerLineage := true } = true := by
  rfl

end CompletionRetry.OutputObligation
