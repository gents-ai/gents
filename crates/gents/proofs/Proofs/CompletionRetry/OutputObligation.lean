namespace CompletionRetry.OutputObligation

inductive Scope
  | request
  | trigger
  deriving DecidableEq, Repr

def active (scope : Scope) (scheduled : Bool) : Bool :=
  match scope with
  | .request => true
  | .trigger => scheduled

structure State where
  minimumWrites : Nat
  completedWrites : Nat
  deriving DecidableEq, Repr

inductive Decision
  | continue
  | complete
  deriving DecidableEq, Repr

def satisfied (state : State) : Prop :=
  state.minimumWrites ≤ state.completedWrites

def decideTerminal (state : State) : Decision :=
  if state.minimumWrites ≤ state.completedWrites then .complete else .continue

def recordWrite (state : State) : State :=
  { state with completedWrites := state.completedWrites + 1 }

theorem unsatisfied_cannot_complete
    (state : State) (h : state.completedWrites < state.minimumWrites) :
    decideTerminal state = .continue := by
  simp [decideTerminal, Nat.not_le.mpr h]

theorem satisfied_can_complete
    (state : State) (h : state.minimumWrites ≤ state.completedWrites) :
    decideTerminal state = .complete := by
  simp [decideTerminal, h]

theorem writes_are_monotone (state : State) :
    state.completedWrites ≤ (recordWrite state).completedWrites := by
  simp [recordWrite]

theorem enough_writes_eventually_complete (minimumWrites : Nat) :
    decideTerminal { minimumWrites, completedWrites := minimumWrites } = .complete := by
  simp [decideTerminal]

theorem trigger_obligation_inactive_for_interactive :
    active .trigger false = false := by
  rfl

theorem request_obligation_active_for_interactive :
    active .request false = true := by
  rfl

end CompletionRetry.OutputObligation
