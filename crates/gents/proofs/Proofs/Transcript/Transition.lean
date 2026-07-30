import Proofs.Transcript.State

namespace Transcript

inductive Transition : TranscriptState → TranscriptState → Prop where
  | append_user {pre post : TranscriptState} {messageId : MessageId} :
      pre.Coherent →
      post = pre.appendUserMessage messageId .ordinary →
      post.Coherent →
      Transition pre post
  | begin_assistant_tool_call {pre post : TranscriptState}
      {callId : ToolExecution.ToolCallId} :
      pre.Coherent →
      callId ∉ pre.inFlight →
      pre.toolCallById? callId = none →
      post = pre.beginAssistantToolCall callId →
      post.Coherent →
      Transition pre post
  | persist_assistant {pre post : TranscriptState}
      {messageId : MessageId} {turn : AssistantTurn} :
      pre.Coherent →
      pre.assistantTurn = some turn →
      post = pre.persistAssistantMessage messageId turn →
      post.Coherent →
      Transition pre post
  | complete_tool_with_result {pre post : TranscriptState}
      {callId : ToolExecution.ToolCallId} {messageId : MessageId} {key : ToolResultKey} :
      pre.Coherent →
      callId ∈ pre.inFlight →
      pre.hasToolResultKey key = false →
      post = pre.completeToolWithResult callId messageId key →
      post.Coherent →
      Transition pre post
  | observe_duplicate_tool_result {pre post : TranscriptState} {key : ToolResultKey} :
      pre.hasToolResultKey key = true →
      post = pre →
      Transition pre post
  | append_distinct_tool_result {pre post : TranscriptState}
      {callId : ToolExecution.ToolCallId} {messageId : MessageId} {key : ToolResultKey} :
      pre.Coherent →
      pre.hasToolResultKey key = false →
      post = pre.appendUserMessage messageId (.toolResult callId key) →
      post.Coherent →
      Transition pre post
  | cancel_in_flight {pre post : TranscriptState} {callId : ToolExecution.ToolCallId} :
      callId ∈ pre.inFlight →
      post = pre.terminalizeInFlight callId .cancelled →
      Transition pre post
  | fail_in_flight {pre post : TranscriptState} {callId : ToolExecution.ToolCallId} :
      callId ∈ pre.inFlight →
      post = pre.terminalizeInFlight callId .failed →
      Transition pre post
  | timeout_in_flight {pre post : TranscriptState} {callId : ToolExecution.ToolCallId} :
      callId ∈ pre.inFlight →
      post = pre.terminalizeInFlight callId .timedOut →
      Transition pre post
  | abandon_hook_ownership {pre post : TranscriptState} :
      post = pre.abandonHookOwnership →
      Transition pre post

inductive Trace : TranscriptState → TranscriptState → Prop where
  | refl {s : TranscriptState} : Trace s s
  | step {s₁ s₂ s₃ : TranscriptState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

end Transcript
