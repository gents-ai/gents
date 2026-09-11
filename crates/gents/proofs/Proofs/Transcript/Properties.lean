import Proofs.Transcript.Transition

namespace Transcript

/-- Ordering follows from the append operation and the pre-state sequence bound,
not an assumed coherent post-state. -/
theorem append_preserves_ordered (s : TranscriptState) (messageId : MessageId)
    (kind : MessageKind) (h_order : s.OrderedBySequence)
    (h_bound : ∀ row ∈ s.messages, row.sequence < s.nextSeq) :
    (s.appendUserMessage messageId kind).OrderedBySequence := by
  unfold TranscriptState.OrderedBySequence TranscriptState.appendUserMessage
  simp only
  unfold TranscriptState.OrderedBySequence at h_order
  generalize hm : s.messages = rows at h_order h_bound ⊢
  clear hm
  induction rows with
  | nil => simp [StrictlyIncreasingMessages]
  | cons row rest ih =>
    rcases h_order with ⟨h_first, h_rest⟩
    refine ⟨?_, ih h_rest (fun other ho => h_bound other (List.mem_cons_of_mem _ ho))⟩
    intro other ho
    rcases List.mem_append.mp ho with ho | ho
    · exact h_first other ho
    · simp only [List.mem_singleton] at ho
      subst other
      exact h_bound row (List.mem_cons_self _ _)

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

theorem complete_tool_with_result_preserves_persisted_reservation
    (s : TranscriptState) (completedCallId : ToolExecution.ToolCallId)
    (messageId : MessageId) (key : ToolResultKey)
    {row : MessageRow} (h_mem : row ∈ s.messages)
    (otherCallId : ToolExecution.ToolCallId) (sessionId : SessionId) (sequence : Sequence)
    (h_reserves : row.reservesToolCall otherCallId sessionId sequence) :
    ∃ row', row' ∈ (s.completeToolWithResult completedCallId messageId key).messages ∧
      row'.reservesToolCall otherCallId sessionId sequence := by
  unfold TranscriptState.completeToolWithResult
  split
  · exact ⟨row, h_mem, h_reserves⟩
  · exact ⟨row, List.mem_append_left _ h_mem, h_reserves⟩

theorem complete_tool_with_result_clears_assistant_turn
    (s : TranscriptState) (callId : ToolExecution.ToolCallId)
    (messageId : MessageId) (key : ToolResultKey)
    (h_fresh : s.hasToolResultKey key = false) :
    (s.completeToolWithResult callId messageId key).assistantTurn = none := by
  simp [TranscriptState.completeToolWithResult, h_fresh]

/-- Duplicate observations execute the same owner and preserve all state,
including sequence allocation and in-flight ownership. -/
theorem duplicate_tool_result_observation_noops (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) (messageId : MessageId) (key : ToolResultKey)
    (h_seen : s.hasToolResultKey key = true) :
    s.completeToolWithResult callId messageId key = s := by
  simp [TranscriptState.completeToolWithResult, h_seen]

theorem complete_tool_with_result_preserves_other_inflight
    (s : TranscriptState)
    (callId otherCallId : ToolExecution.ToolCallId)
    (h_ne : otherCallId ≠ callId)
    (messageId : MessageId) (key : ToolResultKey)
    (h_in : otherCallId ∈ s.inFlight) :
    otherCallId ∈ (s.completeToolWithResult callId messageId key).inFlight := by
  unfold TranscriptState.completeToolWithResult
  split
  · exact h_in
  · simp [Finset.mem_erase, h_ne, h_in]

theorem complete_tool_with_result_preserves_fresh_key
    (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) (messageId : MessageId)
    (key otherKey : ToolResultKey) (h_ne : otherKey ≠ key)
    (h_fresh : s.hasToolResultKey otherKey = false) :
    (s.completeToolWithResult callId messageId key).hasToolResultKey otherKey =
      false := by
  unfold TranscriptState.completeToolWithResult
  split
  · exact h_fresh
  simp only [TranscriptState.hasToolResultKey,
    List.any_append, List.any_cons, List.any_nil, Bool.or_eq_false_iff] at h_fresh ⊢
  refine ⟨h_fresh, ?_, trivial⟩
  simp [MessageRow.isToolResultFor, MessageKind.toolResultKey?, h_ne.symm]

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
