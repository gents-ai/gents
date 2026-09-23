import Proofs.Transcript.Properties
import Mathlib.Data.Finset.Card

namespace Transcript

structure TranscriptCase where
  name : String
  group : String
  action : String
  actionCallIds : List ToolExecution.ToolCallId
  actionLogicalResultIds : List LogicalResultId
  actionPayloadHashes : List PayloadHash
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
  infer_instance

private instance (s : TranscriptState) : Decidable s.DeliveredToolCallsPaired := by
  unfold TranscriptState.DeliveredToolCallsPaired
  have inst (call : ToolCallRow) : Decidable
      (∀ key, call.resultKey = some key → s.toolResultMessageCount key = 1) := by
    cases hk : call.resultKey <;> simp [hk] <;> infer_instance
  exact inferInstance

private instance (s : TranscriptState) : Decidable s.ToolResultMessagesPaired := by
  unfold TranscriptState.ToolResultMessagesPaired
  have inst (row : MessageRow) : Decidable
      (∀ callId key, row.kind = .toolResult callId key → ∃ call ∈ s.toolCalls,
        call.callId = callId ∧ call.resultKey = some key) := by
    cases hk : row.kind <;> simp [hk] <;> infer_instance
  exact inferInstance

private instance (s : TranscriptState) : Decidable s.PairClosed := by
  unfold TranscriptState.PairClosed; infer_instance
private instance (s : TranscriptState) : Decidable s.OrderedBySequence := by
  unfold TranscriptState.OrderedBySequence; infer_instance
private instance (s : TranscriptState) : Decidable s.StrongDrain := by
  unfold TranscriptState.StrongDrain; infer_instance

private def observedCase (name group action : String)
    (actionCallIds : List ToolExecution.ToolCallId)
    (actionLogicalResultIds : List LogicalResultId) (actionPayloadHashes : List PayloadHash)
    (pre post : TranscriptState)
    (key : ToolResultKey) (_trace : Trace pre post) : TranscriptCase :=
  { name, group, action, actionCallIds, actionLogicalResultIds, actionPayloadHashes
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

private def empty : TranscriptState := ⟨0, 1, [], [], ∅⟩
private def key : ToolResultKey := ⟨0, 10, 20⟩
private def user := empty.appendUserMessage 1
private def acceptedTurn : AssistantTurn := ⟨0, 2, [1], .complete⟩
private def published := user.publishAcceptedAssistant 2 acceptedTurn
private def dispatched := published.dispatchToolCall 1
private def completed := dispatched.completeToolWithResult 1 3 key

private theorem ordinaryTrace : Trace empty completed := by
  apply Trace.step (Transition.append_user (messageId := 1) rfl)
  apply Trace.step (Transition.publish_accepted (messageId := 2)
    (turn := acceptedTurn) (by native_decide) rfl rfl)
  apply Trace.step (Transition.dispatch_tool_call (callId := 1) (by native_decide) rfl)
  exact Trace.step (Transition.complete_tool_with_result (callId := 1) (messageId := 3)
    (key := key) (by native_decide) rfl (by native_decide) rfl) Trace.refl

def acceptedPublicationAtomicBeforeDispatchCase : TranscriptCase :=
  observedCase "accepted_publication_atomic_before_dispatch" "publication"
    "publish_complete_header_and_pending_rows" [1] [] [] user published key
    (Trace.step (Transition.publish_accepted (messageId := 2)
      (turn := acceptedTurn) (by native_decide) rfl rfl) Trace.refl)

def orderingUserAssistantToolResultCase : TranscriptCase :=
  observedCase "ordering_user_assistant_tool_result" "ordering"
    "append_user_publish_assistant_dispatch_complete_result" acceptedTurn.callIds
    [key.logicalResultId] [key.payloadHash]
    empty completed key ordinaryTrace

def dedupeDuplicateReusesSequenceCase : TranscriptCase :=
  observedCase "dedupe_duplicate_reuses_sequence" "dedupe" "observe_duplicate_tool_result"
    acceptedTurn.callIds [key.logicalResultId] [key.payloadHash]
    completed (completed.completeToolWithResult 1 4 key) key
    (Trace.step (Transition.observe_duplicate_tool_result (key := key) (by native_decide)
      (duplicate_tool_result_observation_noops completed 1 4 key (by native_decide))) Trace.refl)

private def firstResult := empty.appendUserMessage 1 (.toolResult 1 key)
private def otherKey : ToolResultKey := ⟨0, 11, 20⟩
private def secondResult := firstResult.appendUserMessage 2 (.toolResult 2 otherKey)

/-- Durable orphan results remain representable; pair closure is false here,
and provider sanitation is responsible for narrowing them before inference. -/
def distinctResultIdsAppendDistinctRowsCase : TranscriptCase :=
  observedCase "distinct_result_ids_append_distinct_rows" "dedupe"
    "append_distinct_tool_result" [2] [11] [20] firstResult secondResult otherKey
    (Trace.step (Transition.append_distinct_tool_result (by native_decide) rfl) Trace.refl)

private def parallelTurn : AssistantTurn := ⟨0, 2, [1, 2, 3], .complete⟩
private def parallelPublished := user.publishAcceptedAssistant 2 parallelTurn
private def parallelDispatched :=
  ((parallelPublished.dispatchToolCall 1).dispatchToolCall 2).dispatchToolCall 3
private def parallelKey : ToolResultKey := ⟨0, 30, 40⟩
private def parallelKeyTwo : ToolResultKey := ⟨0, 31, 40⟩
private def parallelKeyThree : ToolResultKey := ⟨0, 32, 40⟩
private def parallelKeys : List ToolResultKey := [parallelKey, parallelKeyTwo, parallelKeyThree]
private def parallelCompleted :=
  ((parallelDispatched.completeToolWithResult 1 3 parallelKey).completeToolWithResult
    2 4 parallelKeyTwo).completeToolWithResult 3 5 parallelKeyThree

private theorem parallelTrace : Trace empty parallelCompleted := by
  apply Trace.step (Transition.append_user (messageId := 1) rfl)
  apply Trace.step (Transition.publish_accepted (messageId := 2)
    (turn := parallelTurn) (by native_decide) rfl rfl)
  apply Trace.step (Transition.dispatch_tool_call (callId := 1) (by native_decide) rfl)
  apply Trace.step (Transition.dispatch_tool_call (callId := 2) (by native_decide) rfl)
  apply Trace.step (Transition.dispatch_tool_call (callId := 3) (by native_decide) rfl)
  apply Trace.step (Transition.complete_tool_with_result (callId := 1) (messageId := 3)
    (key := parallelKey) (by native_decide) rfl (by native_decide) rfl)
  apply Trace.step (Transition.complete_tool_with_result (callId := 2) (messageId := 4)
    (key := parallelKeyTwo) (by native_decide) rfl (by native_decide) rfl)
  exact Trace.step (Transition.complete_tool_with_result (callId := 3) (messageId := 5)
    (key := parallelKeyThree) (by native_decide) rfl (by native_decide) rfl) Trace.refl

def parallelResultsShareAssistantTurnCase : TranscriptCase :=
  observedCase "parallel_results_share_assistant_turn" "ordering"
    "publish_once_dispatch_in_order_then_complete_each_parallel_result"
    parallelTurn.callIds (parallelKeys.map ToolResultKey.logicalResultId)
    (parallelKeys.map ToolResultKey.payloadHash)
    empty parallelCompleted parallelKey parallelTrace

def completedToolPairClosedCase : TranscriptCase :=
  { orderingUserAssistantToolResultCase with
    name := "completed_tool_pair_closed"
    group := "pairing"
    action := "complete_published_tool_with_result" }

private def partialTurn : AssistantTurn := ⟨0, 1, [7, 8], .«partial»⟩
private def partialPublished := empty.publishPartialAssistant 10 partialTurn

def partialPublicationNeverDispatchesCase : TranscriptCase :=
  observedCase "partial_publication_never_dispatches" "publication"
    "publish_partial_header_with_terminal_rows" [7, 8] [] [] empty partialPublished key
    (Trace.step (Transition.publish_partial (messageId := 10)
      (turn := partialTurn) (by native_decide) rfl rfl) Trace.refl)

def unacceptedProviderFailureNoopsCase : TranscriptCase :=
  observedCase "unaccepted_provider_failure_noops" "publication"
    "reject_before_acceptance" [] [] [] empty empty key
    (Trace.step (Transition.reject_unaccepted rfl) Trace.refl)

private def cancelledBeforeDispatch := published.terminalizeToolCall 1 .cancelled

def cancellationAfterPublicationTerminalizesPendingCase : TranscriptCase :=
  observedCase "cancellation_after_publication_terminalizes_pending" "publication"
    "cancel_published_before_dispatch" [1] [] [] published cancelledBeforeDispatch key
    (Trace.step (Transition.cancel_published (callId := 1) (by native_decide) rfl) Trace.refl)

private def failedAfterDispatch := dispatched.terminalizeToolCall 1 .failed

def dispatchFailureRetainsAcceptedHeaderCase : TranscriptCase :=
  observedCase "dispatch_failure_retains_accepted_header" "publication"
    "fail_dispatched_call_without_retracting_header" [1] [] [] published failedAfterDispatch key
    (Trace.step (Transition.dispatch_tool_call (callId := 1) (by native_decide) rfl)
      (Trace.step (Transition.fail_published (callId := 1) (by native_decide) rfl) Trace.refl))

def dropAbandonNotStrongDrainCase : TranscriptCase :=
  observedCase "drop_abandon_not_strong_drain" "hook_boundary" "abandon_hook_ownership"
    [7] [] [] abandonWitnessPre abandonWitnessPost ⟨0, 0, 0⟩
    (Trace.step (Transition.abandon_hook_ownership rfl) Trace.refl)

def transcriptConformanceCases : List TranscriptCase :=
  [ acceptedPublicationAtomicBeforeDispatchCase, orderingUserAssistantToolResultCase
  , dedupeDuplicateReusesSequenceCase, distinctResultIdsAppendDistinctRowsCase
  , parallelResultsShareAssistantTurnCase, completedToolPairClosedCase
  , partialPublicationNeverDispatchesCase, unacceptedProviderFailureNoopsCase
  , cancellationAfterPublicationTerminalizesPendingCase
  , dispatchFailureRetainsAcceptedHeaderCase, dropAbandonNotStrongDrainCase ]

end Transcript
