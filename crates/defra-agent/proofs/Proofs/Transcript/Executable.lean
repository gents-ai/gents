import Proofs.Transcript.Dedupe

/-!
# Transcript Executable Conformance Rows

Finite witness rows for Rust conformance tests. These are case rows, not a
phase-machine table, because transcript correctness is cross-row.
-/

namespace Transcript

structure TranscriptCase where
  name : String
  group : String
  action : String
  legal : Bool
  preMessageCount : Nat
  postMessageCount : Nat
  preToolCallCount : Nat
  postToolCallCount : Nat
  preInFlightCount : Nat
  postInFlightCount : Nat
  assistantSequence : Sequence
  resultSequence : Sequence
  logicalResultId : LogicalResultId
  payloadHash : PayloadHash
  expectedPairClosed : Bool
  expectedOrdered : Bool
  expectedDuplicateReusedSequence : Bool
  expectedStrongDrain : Bool
  deriving Repr

def orderingUserAssistantToolResultCase : TranscriptCase :=
  { name := "ordering_user_assistant_tool_result"
  , group := "ordering"
  , action := "append_user_begin_tool_persist_assistant_complete_result"
  , legal := true
  , preMessageCount := 0
  , postMessageCount := 3
  , preToolCallCount := 0
  , postToolCallCount := 1
  , preInFlightCount := 0
  , postInFlightCount := 0
  , assistantSequence := 2
  , resultSequence := 3
  , logicalResultId := 10
  , payloadHash := 20
  , expectedPairClosed := true
  , expectedOrdered := true
  , expectedDuplicateReusedSequence := false
  , expectedStrongDrain := true
  }

def dedupeDuplicateReusesSequenceCase : TranscriptCase :=
  { name := "dedupe_duplicate_reuses_sequence"
  , group := "dedupe"
  , action := "observe_duplicate_tool_result"
  , legal := true
  , preMessageCount := 3
  , postMessageCount := 3
  , preToolCallCount := 1
  , postToolCallCount := 1
  , preInFlightCount := 0
  , postInFlightCount := 0
  , assistantSequence := 2
  , resultSequence := 3
  , logicalResultId := 10
  , payloadHash := 20
  , expectedPairClosed := true
  , expectedOrdered := true
  , expectedDuplicateReusedSequence := true
  , expectedStrongDrain := true
  }

def distinctResultIdsAppendDistinctRowsCase : TranscriptCase :=
  { name := "distinct_result_ids_append_distinct_rows"
  , group := "dedupe"
  , action := "append_distinct_tool_result"
  , legal := true
  , preMessageCount := 1
  , postMessageCount := 2
  , preToolCallCount := 0
  , postToolCallCount := 0
  , preInFlightCount := 0
  , postInFlightCount := 0
  , assistantSequence := 0
  , resultSequence := 2
  , logicalResultId := 11
  , payloadHash := 20
  , expectedPairClosed := true
  , expectedOrdered := true
  , expectedDuplicateReusedSequence := false
  , expectedStrongDrain := true
  }

def completedToolPairClosedCase : TranscriptCase :=
  { orderingUserAssistantToolResultCase with
    name := "completed_tool_pair_closed"
  , group := "pairing"
  , action := "complete_tool_with_result"
  }

def explicitDrainTerminalizesOwnershipCase : TranscriptCase :=
  { name := "explicit_drain_terminalizes_ownership"
  , group := "hook_boundary"
  , action := "cancel_fail_or_timeout_in_flight"
  , legal := true
  , preMessageCount := 1
  , postMessageCount := 1
  , preToolCallCount := 1
  , postToolCallCount := 1
  , preInFlightCount := 1
  , postInFlightCount := 0
  , assistantSequence := 1
  , resultSequence := 0
  , logicalResultId := 0
  , payloadHash := 0
  , expectedPairClosed := true
  , expectedOrdered := true
  , expectedDuplicateReusedSequence := false
  , expectedStrongDrain := true
  }

def dropAbandonNotStrongDrainCase : TranscriptCase :=
  { name := "drop_abandon_not_strong_drain"
  , group := "hook_boundary"
  , action := "abandon_hook_ownership"
  , legal := true
  , preMessageCount := 0
  , postMessageCount := 0
  , preToolCallCount := 1
  , postToolCallCount := 1
  , preInFlightCount := 1
  , postInFlightCount := 0
  , assistantSequence := 0
  , resultSequence := 0
  , logicalResultId := 0
  , payloadHash := 0
  , expectedPairClosed := false
  , expectedOrdered := true
  , expectedDuplicateReusedSequence := false
  , expectedStrongDrain := false
  }

def transcriptConformanceCases : List TranscriptCase :=
  [ orderingUserAssistantToolResultCase
  , dedupeDuplicateReusesSequenceCase
  , distinctResultIdsAppendDistinctRowsCase
  , completedToolPairClosedCase
  , explicitDrainTerminalizesOwnershipCase
  , dropAbandonNotStrongDrainCase
  ]

end Transcript
