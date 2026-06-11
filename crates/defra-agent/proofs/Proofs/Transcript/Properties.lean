import Proofs.Transcript.Transition

/-!
# Transcript Properties

Local invariant theorems over the transcript transition vocabulary.
-/

namespace Transcript

theorem append_preserves_ordered
    {pre post : TranscriptState}
    (h_step : Transition pre post)
    (h_append : ∃ messageId h_pre h_post_eq h_post_coherent,
      h_step = Transition.append_user
        (messageId := messageId)
        h_pre h_post_eq h_post_coherent) :
    post.OrderedBySequence := by
  rcases h_append with ⟨messageId, h_pre, h_post_eq, h_post_coherent, h_eq⟩
  subst h_eq
  exact h_post_coherent.ordered

theorem append_user_advances_nextSeq
    (s : TranscriptState) (messageId : MessageId) (kind : MessageKind) :
    (s.appendUserMessage messageId kind).nextSeq = s.nextSeq + 1 := by
  rfl

theorem begin_assistant_tool_call_advances_or_reuses_assistant_sequence
    (s : TranscriptState) (callId : ToolExecution.ToolCallId) :
    (s.beginAssistantToolCall callId).nextSeq =
      match s.assistantTurn with
      | some _ => s.nextSeq
      | none => s.nextSeq + 1 := by
  cases h_turn : s.assistantTurn <;>
    simp [TranscriptState.beginAssistantToolCall, h_turn]

theorem tool_call_reserves_assistant_sequence
    {pre post : TranscriptState} {callId : ToolExecution.ToolCallId}
    (_h_post : post = pre.beginAssistantToolCall callId)
    (h_coherent : post.Coherent) :
    ∀ call, call ∈ post.toolCalls → call.callId = callId →
      post.ReservedByPersistedMessage call ∨ post.ReservedByAssistantTurn call := by
  intro call h_mem _
  exact h_coherent.toolCallReservedByMessage call h_mem

theorem persist_assistant_closes_reserved_tool_call_sequence
    {pre post : TranscriptState} {messageId : MessageId} {turn : AssistantTurn}
    (_h_turn : pre.assistantTurn = some turn)
    (h_post : post = pre.persistAssistantMessage messageId turn)
    (callId : ToolExecution.ToolCallId)
    (h_call : callId ∈ turn.callIds) :
    ∃ row, row ∈ post.messages ∧
      row.reservesToolCall callId turn.sessionId turn.sequence := by
  subst post
  refine ⟨
    { messageId := messageId
    , sessionId := turn.sessionId
    , sequence := turn.sequence
    , role := .assistant
    , kind := .assistantToolCalls turn.callIds }, ?_, ?_⟩
  · simp [TranscriptState.persistAssistantMessage]
  · simp [MessageRow.reservesToolCall, MessageKind.referencesToolCall, h_call]

theorem complete_tool_with_result_preserves_coherent
    {pre post : TranscriptState}
    (h_step : Transition pre post)
    (h_complete : ∃ callId messageId key h_pre h_in h_missing h_post_eq h_post_coherent,
      h_step = Transition.complete_tool_with_result
        (callId := callId)
        (messageId := messageId)
        (key := key)
        h_pre h_in h_missing h_post_eq h_post_coherent) :
    post.Coherent := by
  rcases h_complete with
    ⟨callId, messageId, key, h_pre, h_in, h_missing, h_post_eq, h_post_coherent, h_eq⟩
  subst h_eq
  exact h_post_coherent

theorem completed_tool_has_exactly_one_result_message
    {s : TranscriptState} {call : ToolCallRow} {key : ToolResultKey}
    (h_coherent : s.Coherent)
    (h_mem : call ∈ s.toolCalls)
    (h_completed : call.state = .completed)
    (h_key : call.resultKey = some key) :
    s.toolResultMessageCount key = 1 :=
  h_coherent.pairClosed.2.1 call h_mem h_completed key h_key

theorem tool_result_message_has_completed_tool_call
    {s : TranscriptState} {row : MessageRow}
    {callId : ToolExecution.ToolCallId} {key : ToolResultKey}
    (h_coherent : s.Coherent)
    (h_mem : row ∈ s.messages)
    (h_kind : row.kind = .toolResult callId key) :
    ∃ call, call ∈ s.toolCalls ∧
      call.callId = callId ∧
      call.state = .completed ∧
      call.resultKey = some key :=
  h_coherent.pairClosed.2.2 row h_mem callId key h_kind

/-- Completing one tool call's result preserves every reservation already held
by a persisted message: the message list only grows and existing rows are
untouched. Runtime reading: once a multi-call assistant turn is persisted, the
first streamed tool result must NOT revoke the persisted turn that still
reserves the remaining calls. -/
theorem complete_tool_with_result_preserves_persisted_reservation
    (s : TranscriptState)
    (completedCallId : ToolExecution.ToolCallId)
    (messageId : MessageId) (key : ToolResultKey)
    {row : MessageRow} (h_mem : row ∈ s.messages)
    (otherCallId : ToolExecution.ToolCallId)
    (sessionId : SessionId) (sequence : Sequence)
    (h_reserves : row.reservesToolCall otherCallId sessionId sequence) :
    ∃ row', row' ∈ (s.completeToolWithResult completedCallId messageId key).messages ∧
      row'.reservesToolCall otherCallId sessionId sequence :=
  ⟨row, List.mem_append_left _ h_mem, h_reserves⟩

/-- Completing a result clears the in-memory assistant-turn reservation. With
an UNPERSISTED turn this would orphan the turn's sibling calls (post-state
incoherent), which is why `complete_tool_with_result` is only legal once the
turn's reservation lives in a persisted message — the runtime's "cannot persist
streamed tool result before its assistant turn is persisted" guard. -/
theorem complete_tool_with_result_clears_assistant_turn
    (s : TranscriptState) (callId : ToolExecution.ToolCallId)
    (messageId : MessageId) (key : ToolResultKey) :
    (s.completeToolWithResult callId messageId key).assistantTurn = none := rfl

/-- Completing one call leaves a distinct call's in-flight membership intact:
result #1 of a parallel turn does not evict siblings from `inFlight`. -/
theorem complete_tool_with_result_preserves_other_inflight
    (s : TranscriptState)
    (callId otherCallId : ToolExecution.ToolCallId)
    (h_ne : otherCallId ≠ callId)
    (messageId : MessageId) (key : ToolResultKey)
    (h_in : otherCallId ∈ s.inFlight) :
    otherCallId ∈ (s.completeToolWithResult callId messageId key).inFlight := by
  simp [TranscriptState.completeToolWithResult, Finset.mem_erase, h_ne, h_in]

/-- Completing one call with key `key` keeps any distinct key fresh: result #1
does not consume the dedupe freshness of its siblings' keys. -/
theorem complete_tool_with_result_preserves_fresh_key
    (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) (messageId : MessageId)
    (key otherKey : ToolResultKey) (h_ne : otherKey ≠ key)
    (h_fresh : s.hasToolResultKey otherKey = false) :
    (s.completeToolWithResult callId messageId key).hasToolResultKey otherKey =
      false := by
  simp only [TranscriptState.completeToolWithResult, TranscriptState.hasToolResultKey,
    List.any_append, List.any_cons, List.any_nil, Bool.or_eq_false_iff] at h_fresh ⊢
  refine ⟨h_fresh, ?_, trivial⟩
  simp [MessageRow.isToolResultFor, MessageKind.toolResultKey?, h_ne.symm]

/-- Composed multi-result witness: after `persist_assistant`, completing the
first parallel result leaves EVERY `complete_tool_with_result` precondition
intact for a distinct sibling call with a distinct fresh key. This is the
model-side statement that streamed results of one accumulated assistant turn
complete independently; the runtime hook must keep the persisted-turn gate open
for all of them. -/
theorem parallel_results_complete_independently
    (s : TranscriptState)
    (firstCallId siblingCallId : ToolExecution.ToolCallId)
    (h_ne : siblingCallId ≠ firstCallId)
    (messageId : MessageId)
    (firstKey siblingKey : ToolResultKey) (h_key_ne : siblingKey ≠ firstKey)
    (h_sibling_in : siblingCallId ∈ s.inFlight)
    (h_sibling_fresh : s.hasToolResultKey siblingKey = false) :
    siblingCallId ∈ (s.completeToolWithResult firstCallId messageId firstKey).inFlight ∧
      (s.completeToolWithResult firstCallId messageId firstKey).hasToolResultKey
        siblingKey = false :=
  ⟨complete_tool_with_result_preserves_other_inflight
      s firstCallId siblingCallId h_ne messageId firstKey h_sibling_in,
    complete_tool_with_result_preserves_fresh_key
      s firstCallId messageId firstKey siblingKey h_key_ne h_sibling_fresh⟩

theorem explicit_inflight_drain_removes_ownership
    (s : TranscriptState)
    (callId : ToolExecution.ToolCallId)
    (terminal : ToolExecution.ToolCallState) :
    callId ∉ (s.terminalizeInFlight callId terminal).inFlight := by
  simp [TranscriptState.terminalizeInFlight]

def abandonWitnessKey : ToolResultKey :=
  { sessionId := 0, logicalResultId := 0, payloadHash := 0 }

def abandonWitnessToolCall : ToolCallRow :=
  { sessionId := 0
  , callId := 1
  , messageSequence := 0
  , state := .running
  , resultKey := none
  }

def abandonWitnessPre : TranscriptState :=
  { sessionId := 0
  , nextSeq := 1
  , messages := []
  , toolCalls := [abandonWitnessToolCall]
  , inFlight := insert 1 ∅
  , assistantTurn := none
  }

def abandonWitnessPost : TranscriptState :=
  abandonWitnessPre.abandonHookOwnership

theorem abandon_hook_ownership_not_strong_drain :
    Transition abandonWitnessPre abandonWitnessPost ∧
      ¬ abandonWitnessPost.StrongDrain := by
  constructor
  · exact Transition.abandon_hook_ownership rfl
  · intro h_strong
    have h_not_running :=
      h_strong abandonWitnessToolCall
        (by simp [abandonWitnessPost, TranscriptState.abandonHookOwnership, abandonWitnessPre])
    exact h_not_running rfl

end Transcript
