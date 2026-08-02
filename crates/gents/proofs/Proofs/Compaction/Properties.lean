import Proofs.Compaction.Transition

namespace Compaction

theorem uniqueSequences_of_strictlyIncreasing
    {msgs : List Transcript.MessageRow}
    (h : Transcript.StrictlyIncreasingMessages msgs) :
    Transcript.UniqueMessageSequences msgs := by
  induction msgs with
  | nil => trivial
  | cons row rest ih =>
      refine ⟨?_, ?_⟩
      · intro other h_mem h_eq
        have h_lt := h.1 other h_mem
        rw [h_eq] at h_lt
        exact Nat.lt_irrefl other.sequence h_lt
      · exact ih h.2

variable {r : TranscriptReducer} [IsValidReducer r]

theorem reduction_preserves_view_coherent
    {v : PromptView} (h : PromptView.ViewCoherent v) :
    PromptView.ViewCoherent (r v) :=
  IsValidReducer.preservesCoherent (r := r) v h

theorem reduction_preserves_session_id (v : PromptView) :
    (r v).sessionId = v.sessionId :=
  IsValidReducer.preservesSession (r := r) v

theorem reduction_identity_when_below_gate
    {v : PromptView} (h_below : ¬ IsValidReducer.gate (r := r) v) :
    r v = v :=
  IsValidReducer.identityBelowGate (r := r) v h_below

theorem reduction_blocked_unless_safe
    {v : PromptView} (h_unsafe : ¬ PromptView.safeToReduce v) :
    r v = v :=
  IsValidReducer.identityUnlessSafe (r := r) v h_unsafe

theorem reapply_preserves_view_coherent
    {v : PromptView} (h : PromptView.ViewCoherent v) :
    PromptView.ViewCoherent (r (r v)) :=
  IsValidReducer.preservesCoherent (r := r) (r v)
    (IsValidReducer.preservesCoherent (r := r) v h)

/-- Pair closure over a reduced view.

Unlike the pre-#993 version, this is not vacuous: it is the property the real
`summarize` reducer earns by retreating its split to a turn boundary, and
`raw_split_can_orphan` witnesses that an unadjusted split loses it. -/
theorem no_orphaned_tool_results_after_reduction
    {v : PromptView} (h_pre : PromptView.ViewCoherent v) :
    ∀ row, row ∈ (r v).messages →
      ∀ callId key, row.kind = .toolResult callId key →
        ∃ caller, caller ∈ (r v).messages ∧
          caller.role = .assistant ∧
          (∃ callIds, caller.kind = .assistantToolCalls callIds ∧
            callId ∈ callIds) :=
  (IsValidReducer.preservesCoherent (r := r) v h_pre).pairs

theorem retained_window_is_ordered
    {v : PromptView}
    (h_pre : Transcript.StrictlyIncreasingMessages v.messages) :
    Transcript.StrictlyIncreasingMessages (r v).messages :=
  IsValidReducer.preservesOrder (r := r) v h_pre

theorem reduction_idempotent_when_below_gate
    {v : PromptView}
    (h_below : ¬ IsValidReducer.gate (r := r) (r v)) :
    r (r v) = r v :=
  IsValidReducer.identityBelowGate (r := r) (r v) h_below

theorem reduction_idempotent_when_unsafe
    {v : PromptView}
    (h_unsafe : ¬ PromptView.safeToReduce (r v)) :
    r (r v) = r v :=
  IsValidReducer.identityUnlessSafe (r := r) (r v) h_unsafe

theorem reduction_implies_all_retained_tool_results_terminal
    {v : PromptView} (h_nontrivial : r v ≠ v) :
    PromptView.safeToReduce v := by
  by_contra h_unsafe
  exact h_nontrivial (IsValidReducer.identityUnlessSafe (r := r) v h_unsafe)

end Compaction

namespace Compaction

theorem identity_reducer_is_strictly_idempotent (v : PromptView) :
    identityReducer (identityReducer v) = identityReducer v := rfl

end Compaction
