import Proofs.Transcript.Properties
import Mathlib.Data.Finset.Card

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

/-! Fixture observations decide the actual transcript predicates and execute
its mutation functions. They do not assert provider validity of durable rows. -/
private instance (rows : List MessageRow) : Decidable (StrictlyIncreasingMessages rows) := by
  induction rows with
  | nil => simp only [StrictlyIncreasingMessages]; infer_instance
  | cons row rest ih => unfold StrictlyIncreasingMessages; infer_instance

private instance (s : TranscriptState) : Decidable s.ToolCallReservedByMessage := by
  unfold TranscriptState.ToolCallReservedByMessage TranscriptState.ReservedByPersistedMessage
    TranscriptState.ReservedByAssistantTurn
  infer_instance

private instance (s : TranscriptState) : Decidable s.CompletedToolCallsPaired := by
  unfold TranscriptState.CompletedToolCallsPaired
  have inst (call : ToolCallRow) : Decidable
      (∀ key, call.resultKey = some key → s.toolResultMessageCount key = 1) := by
    cases hk : call.resultKey <;> simp [hk] <;> infer_instance
  exact inferInstance

private instance (s : TranscriptState) : Decidable s.ToolResultMessagesPaired := by
  unfold TranscriptState.ToolResultMessagesPaired
  have inst (row : MessageRow) : Decidable
      (∀ callId key, row.kind = .toolResult callId key → ∃ call ∈ s.toolCalls,
        call.callId = callId ∧ call.state = .completed ∧ call.resultKey = some key) := by
    cases hk : row.kind <;> simp [hk] <;> infer_instance
  exact inferInstance

private instance (s : TranscriptState) : Decidable s.PairClosed := by
  unfold TranscriptState.PairClosed; infer_instance
private instance (s : TranscriptState) : Decidable s.OrderedBySequence := by
  unfold TranscriptState.OrderedBySequence; infer_instance
private instance (s : TranscriptState) : Decidable s.StrongDrain := by
  unfold TranscriptState.StrongDrain; infer_instance

private def observedCase (name group action : String) (pre post : TranscriptState)
    (key : ToolResultKey) (_trace : Trace pre post) : TranscriptCase :=
  { name, group, action
  , legal := true -- backed by the required transition trace, not a fixture assertion
  , preMessageCount := pre.messages.length
  , postMessageCount := post.messages.length
  , preToolCallCount := pre.toolCalls.length
  , postToolCallCount := post.toolCalls.length
  , preInFlightCount := pre.inFlight.card
  , postInFlightCount := post.inFlight.card
  , assistantSequence := ((post.messages.find? fun row => row.role == .assistant).map
      MessageRow.sequence).getD 0
  , resultSequence := ((post.messages.find? fun row => row.isToolResultFor key).map
      MessageRow.sequence).getD 0
  , logicalResultId := key.logicalResultId
  , payloadHash := key.payloadHash
  , expectedPairClosed := decide post.PairClosed
  , expectedOrdered := decide post.OrderedBySequence
  , expectedDuplicateReusedSequence := pre.hasToolResultKey key && pre == post
  , expectedStrongDrain := decide post.StrongDrain }

private def empty : TranscriptState := ⟨0, 1, [], [], ∅, none⟩
private def key : ToolResultKey := ⟨0, 10, 20⟩
private def user := empty.appendUserMessage 1
private def pending := user.beginAssistantToolCall 1
private def announced := pending.persistAssistantMessage 2 ⟨0, 2, {1}⟩
private def completed := announced.completeToolWithResult 1 3 key

private theorem ordinaryTrace : Trace empty completed := by
  apply Trace.step (Transition.append_user (messageId := 1) rfl)
  apply Trace.step (Transition.begin_assistant_tool_call (callId := 1) (by decide) (by decide) rfl)
  apply Trace.step (Transition.persist_assistant (messageId := 2) rfl rfl)
  exact Trace.step (Transition.complete_tool_with_result (callId := 1) (messageId := 3)
    (key := key) (by decide) (by decide) rfl) Trace.refl

def orderingUserAssistantToolResultCase : TranscriptCase :=
  observedCase "ordering_user_assistant_tool_result" "ordering"
    "append_user_begin_tool_persist_assistant_complete_result" empty completed key ordinaryTrace

def dedupeDuplicateReusesSequenceCase : TranscriptCase :=
  observedCase "dedupe_duplicate_reuses_sequence" "dedupe" "observe_duplicate_tool_result"
    completed (completed.completeToolWithResult 1 4 key) key
    (Trace.step (Transition.observe_duplicate_tool_result (key := key) (by decide)
      (duplicate_tool_result_observation_noops completed 1 4 key (by decide))) Trace.refl)

private def firstResult := empty.appendUserMessage 1 (.toolResult 1 key)
private def otherKey : ToolResultKey := ⟨0, 11, 20⟩
private def secondResult := firstResult.appendUserMessage 2 (.toolResult 2 otherKey)

/-- Durable orphan results remain representable; pair closure is false here,
and provider sanitation is responsible for narrowing them before inference. -/
def distinctResultIdsAppendDistinctRowsCase : TranscriptCase :=
  observedCase "distinct_result_ids_append_distinct_rows" "dedupe"
    "append_distinct_tool_result" firstResult secondResult otherKey
    (Trace.step (Transition.append_distinct_tool_result (by decide) rfl) Trace.refl)

private def parallelPending := (pending.beginAssistantToolCall 2).beginAssistantToolCall 3
private def parallelAnnounced := parallelPending.persistAssistantMessage 2 ⟨0, 2, {1, 2, 3}⟩
private def parallelKey : ToolResultKey := ⟨0, 30, 40⟩
private def parallelCompleted :=
  ((parallelAnnounced.completeToolWithResult 1 3 parallelKey).completeToolWithResult
    2 4 ⟨0, 31, 40⟩).completeToolWithResult 3 5 ⟨0, 32, 40⟩

private theorem parallelTrace : Trace empty parallelCompleted := by
  apply Trace.step (Transition.append_user (messageId := 1) rfl)
  apply Trace.step (Transition.begin_assistant_tool_call (callId := 1) (by decide) (by decide) rfl)
  apply Trace.step (Transition.begin_assistant_tool_call (callId := 2) (by decide) (by decide) rfl)
  apply Trace.step (Transition.begin_assistant_tool_call (callId := 3) (by decide) (by decide) rfl)
  apply Trace.step (Transition.persist_assistant (messageId := 2) (turn := ⟨0, 2, {1, 2, 3}⟩) (by native_decide) rfl)
  apply Trace.step (Transition.complete_tool_with_result (callId := 1) (messageId := 3)
    (key := parallelKey) (by decide) (by decide) rfl)
  apply Trace.step (Transition.complete_tool_with_result (callId := 2) (messageId := 4)
    (key := ⟨0, 31, 40⟩) (by decide) (by decide) rfl)
  exact Trace.step (Transition.complete_tool_with_result (callId := 3) (messageId := 5)
    (key := ⟨0, 32, 40⟩) (by decide) (by decide) rfl) Trace.refl

def parallelResultsShareAssistantTurnCase : TranscriptCase :=
  observedCase "parallel_results_share_assistant_turn" "ordering"
    "persist_assistant_once_then_complete_each_parallel_result"
    empty parallelCompleted parallelKey parallelTrace

def completedToolPairClosedCase : TranscriptCase :=
  { orderingUserAssistantToolResultCase with
    name := "completed_tool_pair_closed"
    group := "pairing"
    action := "complete_tool_with_result" }

private def drainPre := (empty.beginAssistantToolCall 1).persistAssistantMessage 1 ⟨0, 1, {1}⟩

def explicitDrainTerminalizesOwnershipCase : TranscriptCase :=
  observedCase "explicit_drain_terminalizes_ownership" "hook_boundary"
    "cancel_fail_or_timeout_in_flight" drainPre (drainPre.terminalizeInFlight 1 .cancelled)
    ⟨0, 0, 0⟩ (Trace.step (Transition.cancel_in_flight (by decide) rfl) Trace.refl)

def dropAbandonNotStrongDrainCase : TranscriptCase :=
  observedCase "drop_abandon_not_strong_drain" "hook_boundary" "abandon_hook_ownership"
    abandonWitnessPre abandonWitnessPost ⟨0, 0, 0⟩
    (Trace.step (Transition.abandon_hook_ownership rfl) Trace.refl)

def transcriptConformanceCases : List TranscriptCase :=
  [ orderingUserAssistantToolResultCase, dedupeDuplicateReusesSequenceCase
  , distinctResultIdsAppendDistinctRowsCase, parallelResultsShareAssistantTurnCase
  , completedToolPairClosedCase, explicitDrainTerminalizesOwnershipCase
  , dropAbandonNotStrongDrainCase ]

end Transcript
