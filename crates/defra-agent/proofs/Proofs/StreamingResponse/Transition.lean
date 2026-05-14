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
      post = { pre with liveTail := .nonEmpty, lastProgressAt := pre.now } →
      Transition pre post
  | flushPending
      {pre post : ResponseContext} :
      pre.status = .streaming →
      post = pre →
      Transition pre post
  | resetTail
      {pre post : ResponseContext} :
      pre.status = .streaming →
      post = { pre with liveTail := .empty } →
      Transition pre post
  | setInterruptedAt
      {pre post : ResponseContext} {t : Time} :
      pre.interruptedAt = none →
      post = { pre with interruptedAt := some t } →
      Transition pre post

end StreamingResponse
