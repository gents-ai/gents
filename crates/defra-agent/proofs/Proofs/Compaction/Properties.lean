import Proofs.Compaction.Transition

/-!
# Compaction Properties

Contract-parametric theorems over any `IsValidReducer` instance.
Every theorem here is parametric over `r : TranscriptReducer` with
`[IsValidReducer r]` -- any future strategy that instantiates the
typeclass picks up these theorems for free.

Witness-specific theorems (strict idempotence for identityReducer and
stripToolResultsReducer) live at the bottom of the file under a
dedicated section header.
-/

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

/-- Re-coherence: a valid reducer applied to a coherent view produces
a coherent view. Composes the preservation obligations. -/
theorem reduction_preserves_view_coherent
    {v : PromptView} (h : PromptView.ViewCoherent v) :
    PromptView.ViewCoherent (r v) := by
  refine ⟨?_, ?_, ?_⟩
  · exact IsValidReducer.preservesPairs (r := r) v h.pairs
  · exact IsValidReducer.preservesOrder (r := r) v h.ordered
  · exact uniqueSequences_of_strictlyIncreasing
      (IsValidReducer.preservesOrder (r := r) v h.ordered)

/-- Session identity is preserved by any valid reducer. -/
theorem reduction_preserves_session_id (v : PromptView) :
    (r v).sessionId = v.sessionId :=
  IsValidReducer.preservesSession (r := r) v

/-- Below the strategy's gate, the reducer is the identity. -/
theorem reduction_identity_when_below_gate
    {v : PromptView} (h_below : ¬ IsValidReducer.gate (r := r) v) :
    r v = v :=
  IsValidReducer.identityBelowGate (r := r) v h_below

/-- When the view is not safe to reduce (some retained tool-result
message belongs to a non-terminal streaming response), the reducer
must be the identity. -/
theorem reduction_blocked_unless_safe
    {v : PromptView} (h_unsafe : ¬ PromptView.safeToReduce v) :
    r v = v :=
  IsValidReducer.identityUnlessSafe (r := r) v h_unsafe

/-- Invariant idempotence: re-applying a reducer preserves `ViewCoherent`.
The strict `r (r v) = r v` form would fail for LLM strategies; this is
the safety-preserving weak form. -/
theorem reapply_preserves_view_coherent
    {v : PromptView} (h : PromptView.ViewCoherent v) :
    PromptView.ViewCoherent (r (r v)) :=
  IsValidReducer.reapplyPreservesCoh (r := r) v h

/-- Acceptance criterion (a) from issue #184: no orphaned `AgentToolCall`
rows after compaction. Direct corollary of `preservesPairs`. -/
theorem no_orphaned_tool_results_after_reduction
    {v : PromptView}
    (h_pre : PromptView.PairsClosedInMessages v.messages) :
    ∀ row, row ∈ (r v).messages →
      ∀ callId key, row.kind = .toolResult callId key →
        ∃ caller, caller ∈ (r v).messages ∧
          caller.role = .assistant ∧
          (∃ callIds, caller.kind = .assistantToolCalls callIds ∧
            callId ∈ callIds) := by
  intro row h_mem callId key h_kind
  exact IsValidReducer.preservesPairs (r := r) v h_pre row h_mem callId key h_kind

/-- Acceptance criterion (b) from issue #184: message-order monotonicity
within retained windows. Direct corollary of `preservesOrder`. -/
theorem retained_window_is_ordered
    {v : PromptView}
    (h_pre : Transcript.StrictlyIncreasingMessages v.messages) :
    Transcript.StrictlyIncreasingMessages (r v).messages :=
  IsValidReducer.preservesOrder (r := r) v h_pre

/-- Acceptance criterion (c) from issue #184: idempotence under
re-application -- conditional form 1. When the once-reduced view falls
below the gate, re-application is a strict no-op. -/
theorem reduction_idempotent_when_below_gate
    {v : PromptView}
    (h_below : ¬ IsValidReducer.gate (r := r) (r v)) :
    r (r v) = r v :=
  IsValidReducer.identityBelowGate (r := r) (r v) h_below

/-- Acceptance criterion (c) from issue #184: idempotence under
re-application -- conditional form 2. When the once-reduced view is
no longer safe to reduce, re-application is a strict no-op. -/
theorem reduction_idempotent_when_unsafe
    {v : PromptView}
    (h_unsafe : ¬ PromptView.safeToReduce (r v)) :
    r (r v) = r v :=
  IsValidReducer.identityUnlessSafe (r := r) (r v) h_unsafe

/-- Streaming-coupling theorem: any non-identity reduction implies every
tool-result message in the input view has a terminal streaming-response
status. Ties compaction to #190's `Status.isTerminal` vocabulary. -/
theorem reduction_implies_all_retained_tool_results_terminal
    {v : PromptView} (h_nontrivial : r v ≠ v) :
    PromptView.safeToReduce v := by
  by_contra h_unsafe
  exact h_nontrivial (IsValidReducer.identityUnlessSafe (r := r) v h_unsafe)

end Compaction
