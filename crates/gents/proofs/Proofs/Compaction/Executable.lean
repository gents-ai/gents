import Proofs.Compaction.Summarize

namespace Compaction

/-- The fixtures the Rust conformance driver builds, keyed by row count.

`tests/conformance/streaming_compaction.rs::compaction_messages_for_case`
constructs the same shapes out of `gents::llm::message::Message`, so the
`safeBoundary` values below are computed from the model and checked against
production's `compaction::pair_safe_boundary` on the corresponding Rust
fixture. -/
def caseFixture : Nat → List Transcript.MessageRow
  | 1 => [⟨0, 0, 0, .user, .toolResult 1 ⟨0, 0, 0⟩⟩]
  | 2 => [⟨0, 0, 0, .assistant, .assistantToolCalls {1}⟩,
          ⟨1, 0, 1, .user, .toolResult 1 ⟨0, 0, 0⟩⟩]
  | 3 => [⟨0, 0, 0, .user, .ordinary⟩,
          ⟨1, 0, 1, .assistant, .assistantToolCalls {1}⟩,
          ⟨2, 0, 2, .user, .toolResult 1 ⟨0, 0, 0⟩⟩]
  | _ => []

/-- The straddling split: a budget index of 2 lands between the assistant
announcement and its result, and the boundary retreats to 1 so the turn stays
whole in the retained tail. -/
theorem caseFixture_boundaries_pinned :
    pairSafeBoundary (caseFixture 3) 2 = 1 ∧
      pairSafeBoundary (caseFixture 3) 1 = 1 ∧
      pairSafeBoundary (caseFixture 3) 3 = 3 ∧
      pairSafeBoundary (caseFixture 2) 1 = 0 := by
  refine ⟨?_, ?_, ?_, ?_⟩ <;> decide

structure CompactionReducerCase where
  name                : String
  group               : String
  reducer             : String
  legal               : Bool
  preMessageCount     : Nat
  postMessageCount    : Nat
  preservesPairs      : Bool
  preservesOrder      : Bool
  gateOpen            : Bool
  safeToReduce        : Bool
  reducerIsIdentity   : Bool
  /-- The raw token-budget index production computes before any adjustment. -/
  splitIndex          : Nat
  /-- Where the boundary lands after retreating to a turn boundary. Computed by
  `pairSafeBoundary`, not asserted. -/
  safeBoundary        : Nat
  /-- Rows surviving the reduction. -/
  retainedCount       : Nat
  deriving Repr

def compactionReducerCases : List CompactionReducerCase := [
  { name              := "identity_reducer_is_no_op"
  , group             := "witness"
  , reducer           := "identity"
  , legal             := true
  , preMessageCount   := 0
  , postMessageCount  := 0
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 0 }
, { name              := "identity_preserves_pair_atomicity"
  , group             := "witness"
  , reducer           := "identity"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 2 }
, { name              := "identity_preserves_message_order"
  , group             := "witness"
  , reducer           := "identity"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 3 }
, { name              := "strip_preserves_pair_atomicity"
  , group             := "witness"
  , reducer           := "strip"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 2 }
, { name              := "strip_preserves_message_order"
  , group             := "witness"
  , reducer           := "strip"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 3 }
, { name              := "strip_is_strictly_idempotent"
  , group             := "witness"
  , reducer           := "strip"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 2 }
, { name              := "reduction_blocked_when_response_streaming"
  , group             := "streaming"
  , reducer           := "any_valid"
  , legal             := true
  , preMessageCount   := 1
  , postMessageCount  := 1
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := false
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 1 }
, { name              := "reduction_allowed_when_response_terminal"
  , group             := "streaming"
  , reducer           := "any_valid"
  , legal             := true
  , preMessageCount   := 1
  , postMessageCount  := 1
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := false
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 1 }
, { name              := "no_orphaned_tool_results_after_strip"
  , group             := "contract"
  , reducer           := "strip"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 2 }
, { name              := "reapply_preserves_view_coherent"
  , group             := "contract"
  , reducer           := "any_valid"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 2 }
  -- The straddling split: the raw budget index falls between the assistant
  -- announcement and its result. Left alone it orphans the result
  -- (`raw_split_can_orphan`); the boundary retreats so the turn stays whole.
, { name              := "summarize_retains_straddling_turn"
  , group             := "summarize"
  , reducer           := "summarize"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := false
  , splitIndex        := 2
  , safeBoundary      := pairSafeBoundary (caseFixture 3) 2
  , retainedCount     := 2 }
, { name              := "summarize_drops_whole_turns"
  , group             := "summarize"
  , reducer           := "summarize"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := false
  , splitIndex        := 1
  , safeBoundary      := pairSafeBoundary (caseFixture 3) 1
  , retainedCount     := 2 }
  -- If the complete assistant-call/result tail is larger than the retention
  -- budget, production advances to the boundary after the pair and summarizes
  -- every row. The retained provider view is empty rather than oversized or
  -- orphaned; the generated summary becomes the next prompt.
, { name              := "summarize_oversized_complete_turn"
  , group             := "summarize"
  , reducer           := "summarize"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 0
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := false
  , splitIndex        := 3
  , safeBoundary      := pairSafeBoundary (caseFixture 3) 3
  , retainedCount     := 0 }
  -- The gate is production's, not the test's: safe_to_reduce is the function
  -- under test, and a closed gate must leave the transcript untouched.
, { name              := "summarize_blocked_when_response_streaming"
  , group             := "summarize"
  , reducer           := "summarize"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := false
  , reducerIsIdentity := true
  , splitIndex        := 2
  , safeBoundary      := pairSafeBoundary (caseFixture 3) 2
  , retainedCount     := 3 }
  -- The boundary cannot retreat past the head of a turn that opens the window.
, { name              := "summarize_cannot_split_a_leading_turn"
  , group             := "summarize"
  , reducer           := "summarize"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 1
  , safeBoundary      := pairSafeBoundary (caseFixture 2) 1
  , retainedCount     := 2 }
, { name              := "provider_view_is_idempotent"
  , group             := "provider_view"
  , reducer           := "provider_view"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 3 }
  -- An orphaned result at the head is removed by the view, so the unsanitized
  -- and sanitized indexings of a compacted prefix diverge. Defect 3 of #993.
, { name              := "provider_view_drops_orphaned_result"
  , group             := "provider_view"
  , reducer           := "provider_view"
  , legal             := true
  , preMessageCount   := 1
  , postMessageCount  := 0
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := false
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 0 }
]

theorem compactionReducerCases_count :
    compactionReducerCases.length = 17 := by decide

end Compaction
