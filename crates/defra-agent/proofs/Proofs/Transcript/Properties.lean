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
