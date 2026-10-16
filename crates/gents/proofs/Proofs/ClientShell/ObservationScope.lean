namespace ClientObservationScope

structure State where
  requested : Option Nat
  attachments : Nat
  deriving DecidableEq, Repr

inductive Action where
  | select (node : Option Nat)
  | attach
  | detach
  deriving DecidableEq, Repr

/-- An attached frontend may observe another node than the embedding shell.
The shared observer must retain the full authorized scope until it detaches;
this changes observation focus, never document authorization. -/
def effective (state : State) : Option Nat :=
  if state.attachments = 0 then state.requested else none

def step (state : State) : Action → State
  | .select node => { state with requested := node }
  | .attach => { state with attachments := state.attachments + 1 }
  | .detach => { state with attachments := state.attachments - 1 }

theorem attached_scope_is_open (state : State) (h : 0 < state.attachments) :
    effective state = none := by
  simp [effective, Nat.ne_of_gt h]

theorem selection_cannot_narrow_an_attached_observer
    (state : State) (node : Option Nat) (h : 0 < state.attachments) :
    effective (step state (.select node)) = none := by
  simp [step, effective, Nat.ne_of_gt h]

theorem last_detachment_restores_current_intent
    (state : State) (h : state.attachments = 1) :
    effective (step state .detach) = state.requested := by
  simp [step, effective, h]

theorem attachment_and_detachment_preserve_focus (state : State) :
    effective (step (step state .attach) .detach) = effective state := by
  simp [step]

theorem selection_during_attachment_survives_detachment
    (state : State) (node : Option Nat) (h : state.attachments = 1) :
    effective (step (step state (.select node)) .detach) = node := by
  simp [step, effective, h]

theorem detached_count_stays_zero (node : Option Nat) :
    step { requested := node, attachments := 0 } .detach =
      { requested := node, attachments := 0 } := by
  rfl

end ClientObservationScope
