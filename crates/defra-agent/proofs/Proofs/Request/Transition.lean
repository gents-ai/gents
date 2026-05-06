import Proofs.Request.State

/-!
# Request Transitions

Relational request lifecycle transitions.
-/

namespace RequestContext

/-- Request lifecycle transitions. Each constructor encodes one valid
    state transition together with the resulting record update.

    `inputRequired` remains part of the persisted vocabulary, but the current
    Rust product does not emit it. The core transition relation therefore has
    no active path into or out of that reserved state. -/
inductive Transition : RequestContext → RequestContext → Prop where
  | claim {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
      pre.ttlOpen →
      post = { pre with state := .claimed, admission := .waiting, claimTime := pre.currentTime, deadline := pre.currentTime + 1 } →
      Transition pre post
  | dedup_lose {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
      post = { pre with state := .superseded } →
      Transition pre post
  | begin_inference {pre post : RequestContext} :
      pre.state = .claimed →
      pre.admission = .acquired →
      post = { pre with state := .processing, admission := .executing } →
      Transition pre post
  | advance {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      post = { pre with progressSeq := pre.progressSeq + 1 } →
      Transition pre post
  | finish {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      post = { pre with state := .completed, admission := .released, persistence := .committed } →
      Transition pre post
  | fail {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      post = { pre with state := .failed, admission := .released } →
      Transition pre post
  | fail_before_stream {pre post : RequestContext} :
      pre.state = .claimed →
      (pre.admission = .waiting ∨ pre.admission = .acquired) →
      post = { pre with state := .failed, admission := .released } →
      Transition pre post
  | expire {pre post : RequestContext} {t : Time} :
      pre.state = .pending →
      pre.admission = .released →
      pre.validUntil = some t →
      pre.currentTime > t →
      post = { pre with state := .dead, admission := .released } →
      Transition pre post
  | interrupt_before_claim {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      Transition pre post
  | interrupt_claimed {pre post : RequestContext} :
      pre.state = .claimed →
      (pre.admission = .waiting ∨ pre.admission = .acquired) →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      Transition pre post
  | interrupt_processing {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      Transition pre post


end RequestContext
