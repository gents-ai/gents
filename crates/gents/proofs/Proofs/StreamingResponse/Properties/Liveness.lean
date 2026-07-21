import Proofs.StreamingResponse.Transition

/-! Constructive terminal paths for streaming and stale-recovery response states. -/

namespace StreamingResponse

theorem streamIdle_eventually_terminal
    (pre : ResponseContext)
    (h_streaming : pre.status = .streaming)
    (h_expired : pre.now > pre.streamIdleDeadline) :
    ∃ post, Transition pre post ∧ post.status = .error ∧
            post.errorReason = some .streamIdleTimeout := by
  refine ⟨{ pre with
    status := .error
  , liveTail := .empty
  , errorReason := some .streamIdleTimeout }, ?_, ?_, ?_⟩
  · exact Transition.finalizeError h_streaming
      (Or.inr (Or.inr (Or.inl rfl)))
      (fun _ => h_expired)
      rfl
  · rfl
  · rfl

theorem streaming_eventually_terminal
    (pre : ResponseContext)
    (h_streaming : pre.status = .streaming) :
    ∃ post, Transition pre post ∧ isTerminal post.status := by
  refine ⟨{ pre with
    status := .error
  , errorReason := some .daemonRestartRecovery }, ?_, ?_⟩
  · exact Transition.recoverInterrupted h_streaming rfl
  · exact Or.inr rfl

/-- Parity: the streaming-stale recovery condition reaches an error
state via the canonical `Transition.recoverInterrupted`. This is the
formal statement that `responseRecoverySweep` in `Recovery/Sweeps.lean`
is a degenerate instance of the transition relation. -/
theorem recoverInterrupted_constructible
    (pre : ResponseContext)
    (h_streaming : pre.status = .streaming) :
    ∃ post, Transition pre post ∧
            post.status = .error ∧
            post.errorReason = some .daemonRestartRecovery := by
  refine ⟨{ pre with
    status := .error
  , errorReason := some .daemonRestartRecovery }, ?_, ?_, ?_⟩
  · exact Transition.recoverInterrupted h_streaming rfl
  · rfl
  · rfl

end StreamingResponse
