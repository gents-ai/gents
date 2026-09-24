import Proofs.Transcript.State

namespace Transcript

/-!
The output owner supplies only closure-validated assistant publications to this
surface. `publish_accepted` is the Complete closure/header/pending-row commit;
`publish_partial` is the terminal Partial publication. Segment bytes, closure
selection, reconstruction, and transaction mechanics remain in
`CanonicalOutput`. Dispatch constructors represent the later lifecycle decision
after the request/tool owner has applied cancellation, deadline, and policy
guards.

In particular, `turn.outcome = .complete` classifies the supplied header; it
does not prove closure validity. Likewise `ReadyToDispatch` proves publication
and local row order, not request/tool expiry, cancellation, or policy
authorization. Those are preconditions supplied by their existing owners.

Publication also assumes the bridge has allocated and genesis-validated a
fresh immutable Defra message identity. The transition retains old rows by
value; it intentionally does not claim that an equal `messageId` was rejected.
-/

inductive Transition : TranscriptState → TranscriptState → Prop where
  | append_user {pre post : TranscriptState} {messageId : MessageId} :
      post = pre.appendUserMessage messageId .ordinary →
      Transition pre post
  | publish_accepted {pre post : TranscriptState}
      {messageId : MessageId} {turn : AssistantTurn} :
      pre.PublishableTurn turn →
      turn.outcome = .complete →
      post = pre.publishAcceptedAssistant messageId turn →
      Transition pre post
  | publish_partial {pre post : TranscriptState}
      {messageId : MessageId} {turn : AssistantTurn} :
      pre.PublishableTurn turn →
      turn.outcome = .«partial» →
      post = pre.publishPartialAssistant messageId turn →
      Transition pre post
  /-- Failure before acceptance has no transcript or tool-lifecycle effect. -/
  | reject_unaccepted {pre post : TranscriptState} :
      post = pre →
      Transition pre post
  | dispatch_tool_call {pre post : TranscriptState}
      {callId : ToolExecution.ToolCallId} :
      pre.ReadyToDispatch callId →
      post = pre.dispatchToolCall callId →
      Transition pre post
  | complete_tool_with_result {pre post : TranscriptState}
      {callId : ToolExecution.ToolCallId} {messageId : MessageId} {key : ToolResultKey} :
      pre.RunningPublishedCall callId →
      key.sessionId = pre.sessionId →
      pre.hasToolResultKey key = false →
      post = pre.completeToolWithResult callId messageId key →
      Transition pre post
  | observe_duplicate_tool_result {pre post : TranscriptState} {key : ToolResultKey} :
      pre.hasToolResultKey key = true →
      post = pre →
      Transition pre post
  /-- Permissive durable observations may contain an orphan result. Provider
  input still narrows those observations before inference. -/
  | append_distinct_tool_result {pre post : TranscriptState}
      {callId : ToolExecution.ToolCallId} {messageId : MessageId} {key : ToolResultKey} :
      pre.hasToolResultKey key = false →
      post = pre.appendUserMessage messageId (.toolResult callId key) →
      Transition pre post
  /-- Applies to both undispatched pending rows and dispatched running rows. -/
  | cancel_published {pre post : TranscriptState} {callId : ToolExecution.ToolCallId} :
      pre.CancellablePublishedCall callId →
      post = pre.terminalizeToolCall callId .cancelled →
      Transition pre post
  | fail_published {pre post : TranscriptState} {callId : ToolExecution.ToolCallId} :
      pre.CancellablePublishedCall callId →
      post = pre.terminalizeToolCall callId .failed →
      Transition pre post
  | timeout_published {pre post : TranscriptState} {callId : ToolExecution.ToolCallId} :
      pre.CancellablePublishedCall callId →
      post = pre.terminalizeToolCall callId .timedOut →
      Transition pre post
  | abandon_hook_ownership {pre post : TranscriptState} :
      post = pre.abandonHookOwnership →
      Transition pre post

inductive Trace : TranscriptState → TranscriptState → Prop where
  | refl {s : TranscriptState} : Trace s s
  | step {s₁ s₂ s₃ : TranscriptState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

end Transcript
