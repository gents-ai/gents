import Proofs.Transcript.Properties

namespace Transcript

theorem duplicate_tool_result_observation_noops
    {pre post : TranscriptState} {key : ToolResultKey}
    (_h_seen : pre.hasToolResultKey key = true)
    (h_post : post = pre) :
    post = pre := h_post

theorem dedupe_exactly_one_row_per_key
    {s : TranscriptState} {key : ToolResultKey}
    (h_coherent : s.Coherent)
    (call : ToolCallRow)
    (h_mem : call ∈ s.toolCalls)
    (h_completed : call.state = .completed)
    (h_key : call.resultKey = some key) :
    s.toolResultMessageCount key = 1 :=
  completed_tool_has_exactly_one_result_message
    h_coherent h_mem h_completed h_key

theorem distinct_tool_result_keys_append_distinct_rows
    (s : TranscriptState)
    (messageId₁ messageId₂ : MessageId)
    (callId₁ callId₂ : ToolExecution.ToolCallId)
    (key₁ key₂ : ToolResultKey)
    (_h_distinct : key₁ ≠ key₂) :
    (s.appendUserMessage messageId₁ (.toolResult callId₁ key₁)).messages.length + 1 =
      (TranscriptState.appendUserMessage
        (s.appendUserMessage messageId₁ (.toolResult callId₁ key₁))
        messageId₂
        (.toolResult callId₂ key₂)).messages.length := by
  simp [TranscriptState.appendUserMessage]

theorem toolResultKey_session_scoped
    {left right : ToolResultKey}
    (h_eq : left = right) :
    left.sessionId = right.sessionId := by
  rw [h_eq]

end Transcript
