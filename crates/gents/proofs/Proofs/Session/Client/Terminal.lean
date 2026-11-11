import Proofs.Client.Lifecycle

def effectivelyTerminal (view : AttemptView) : Prop :=
  view.request.isSuperseded = true ∨
  view.request.lifecycleState = .completed ∨
  view.request.lifecycleState = .failed ∨
  view.request.lifecycleState = .superseded ∨
  view.request.lifecycleState = .dead ∨
  view.request.lifecycleState = .interrupted

instance (view : AttemptView) : Decidable (effectivelyTerminal view) := by
  unfold effectivelyTerminal
  infer_instance

theorem terminal_coherence (view : AttemptView) :
    (deriveAttempt view).isTerminal = true ↔ effectivelyTerminal view := by
  obtain ⟨⟨state, isSuperseded⟩⟩ := view
  cases isSuperseded <;> cases state <;>
    simp [deriveAttempt, ClientTurnState.isTerminal, effectivelyTerminal]
