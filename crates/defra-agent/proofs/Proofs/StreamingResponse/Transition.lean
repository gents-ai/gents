import Proofs.StreamingResponse.State

/-!
# StreamingResponse Transitions

Relational transitions for the AgentResponse streaming → terminal
lifecycle, plus the composed `BridgeTransition` for the S6 bridge.
-/

namespace StreamingResponse

inductive Transition : ResponseContext → ResponseContext → Prop where
  | begin
      {pre post : ResponseContext} :
      pre.status = .streaming →
      pre.liveTail = .empty →
      pre.tokenCount = 0 →
      pre.materializedMessageSequence = none →
      post = pre →
      Transition pre post
  | writeTokens
      {pre post : ResponseContext} {delta : Nat} :
      pre.status = .streaming →
      delta > 0 →
      post = { pre with
        liveTail := .nonEmpty
      , tokenCount := pre.tokenCount + delta
      , lastProgressAt := pre.now } →
      Transition pre post
  | writeReasoning
      {pre post : ResponseContext} :
      pre.status = .streaming →
      post = { pre with
        liveTail := .nonEmpty
      , tailReasoning := .nonEmpty
      , lastProgressAt := pre.now } →
      Transition pre post
  | flushPending
      {pre post : ResponseContext} :
      pre.status = .streaming →
      post = pre →
      Transition pre post
  | resetTail
      {pre post : ResponseContext} :
      pre.status = .streaming →
      post = { pre with liveTail := .empty, tailReasoning := .empty } →
      Transition pre post
  | setInterruptedAt
      {pre post : ResponseContext} {t : Time} :
      pre.status = .streaming →
      pre.interruptedAt = none →
      post = { pre with interruptedAt := some t } →
      Transition pre post
  | finalizeComplete
      {pre post : ResponseContext} {seq : Transcript.Sequence} :
      pre.status = .streaming →
      post = { pre with
        status := .completed
      , liveTail := .empty
      -- issue #492: copy-then-clear. The reasoning present in the live tail
      -- is durably persisted into the materialized message BEFORE the live
      -- tail is cleared. `durableReasoning` captures that copy; `liveTail`
      -- still clears to `.empty` (issue #64 invariant preserved).
      , durableReasoning := pre.tailReasoning
      , materializedMessageSequence := some seq } →
      Transition pre post
  | finalizeError
      {pre post : ResponseContext} {reason : ErrorReason} :
      pre.status = .streaming →
      (reason = .inferenceFailed ∨ reason = .finalizeRequestedError ∨
       reason = .streamIdleTimeout ∨ reason = .interrupted) →
      (reason = .streamIdleTimeout → pre.now > pre.streamIdleDeadline) →
      post = { pre with
        status := .error
      , liveTail := .empty
      , errorReason := some reason } →
      Transition pre post
  | recoverInterrupted
      {pre post : ResponseContext} :
      pre.status = .streaming →
      post = { pre with
        status := .error
      , errorReason := some .daemonRestartRecovery } →
      Transition pre post
  | observeIdempotentFinalize
      {pre post : ResponseContext} :
      (pre.status = .completed ∨ pre.status = .error) →
      post = pre →
      Transition pre post

inductive Trace : ResponseContext → ResponseContext → Prop where
  | refl {s : ResponseContext} : Trace s s
  | step {s₁ s₂ s₃ : ResponseContext} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

inductive BridgeTransition : ResponseRequestBridge → ResponseRequestBridge → Prop where
  | finalizeComplete
      {pre post : ResponseRequestBridge} {seq : Transcript.Sequence} :
      pre.response.status = .streaming →
      post.response = { pre.response with
        status := .completed
      , liveTail := .empty
      , durableReasoning := pre.response.tailReasoning
      , materializedMessageSequence := some seq } →
      pre.requestState = .processing →
      post.requestState = .completed →
      post.requestPersistence = .committed →
      BridgeTransition pre post
  | finalizeError
      {pre post : ResponseRequestBridge} {reason : ErrorReason} :
      pre.response.status = .streaming →
      (reason = .inferenceFailed ∨ reason = .finalizeRequestedError ∨
       reason = .streamIdleTimeout ∨ reason = .interrupted) →
      (reason = .streamIdleTimeout →
         pre.response.now > pre.response.streamIdleDeadline) →
      post.response = { pre.response with
        status := .error
      , liveTail := .empty
      , errorReason := some reason } →
      pre.requestState = .processing →
      post.requestState = .failed →
      post.requestPersistence = .committed →
      BridgeTransition pre post
  | recoverPaired
      {pre post : ResponseRequestBridge} :
      pre.response.status = .streaming →
      post.response = { pre.response with
        status := .error
      , errorReason := some .daemonRestartRecovery } →
      pre.requestState = .processing →
      post.requestState = .failed →
      post.requestPersistence = .committed →
      BridgeTransition pre post

end StreamingResponse
