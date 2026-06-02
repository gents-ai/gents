import Proofs.CodexShim.Projection

/-!
# Codex Shim Local Interrupt Coherence

The Codex shim may locally acknowledge an interrupt before the core request row
has reached `interrupted`, but only while the observed request is still
interruptible from the adapter boundary.
-/

namespace CodexShim

/-- Request states where a local Codex interrupt acknowledgement is sound. -/
def interruptibleRequestState : RequestState → Prop
  | .processing => True
  | .inputRequired => True
  | _ => False

instance (s : RequestState) : Decidable (interruptibleRequestState s) := by
  cases s <;> simp [interruptibleRequestState] <;> infer_instance

/-- Well-formed observations do not acknowledge local interrupts for
non-interruptible request states. -/
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

/-- The local interrupt shortcut is terminal and not premature for coherent
observations: acknowledging the interrupt implies an interruptible request state
and projects to a terminal Codex phase. -/
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
