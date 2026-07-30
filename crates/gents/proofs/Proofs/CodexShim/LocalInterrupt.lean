import Proofs.CodexShim.Projection

namespace CodexShim

def interruptibleRequestState : RequestState → Prop
  | .processing => True
  | .inputRequired => True
  | _ => False

instance (s : RequestState) : Decidable (interruptibleRequestState s) := by
  cases s <;> simp [interruptibleRequestState] <;> infer_instance

def localInterruptCoherent (obs : ProjectionObservation) : Prop :=
  obs.localInterruptAcked = true → interruptibleRequestState obs.requestState

instance (obs : ProjectionObservation) : Decidable (localInterruptCoherent obs) := by
  unfold localInterruptCoherent
  infer_instance

theorem local_interrupt_requires_interruptible
    {obs : ProjectionObservation}
    (h_sound : localInterruptCoherent obs)
    (h_ack : obs.localInterruptAcked = true) :
    interruptibleRequestState obs.requestState :=
  h_sound h_ack

theorem local_interrupt_shortcut_sound
    {obs : ProjectionObservation}
    (h_sound : localInterruptCoherent obs)
    (h_ack : obs.localInterruptAcked = true) :
    TurnPhase.terminal (projectObservation obs) ∧
      interruptibleRequestState obs.requestState := by
  exact
    ⟨ by
        rw [local_interrupt_projects_interrupted h_ack]
        trivial
    , local_interrupt_requires_interruptible h_sound h_ack
    ⟩

end CodexShim
