import Proofs.Request.State

/-!
# Request Transitions

Relational request lifecycle transitions.
-/

namespace RequestContext

/-- Request lifecycle transitions. Each constructor encodes one valid
    state transition together with the resulting record update. -/
inductive Transition : RequestContext → RequestContext → Prop where
  | claim {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
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
  | need_input {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      post = { pre with state := .inputRequired } →
      Transition pre post
  | input_received {pre post : RequestContext} :
      pre.state = .inputRequired →
      pre.admission = .executing →
      post = { pre with state := .processing, progressSeq := pre.progressSeq + 1 } →
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
  | input_timeout {pre post : RequestContext} :
      pre.state = .inputRequired →
      pre.admission = .executing →
      pre.deadlineExceeded →
      post = { pre with state := .failed, admission := .released } →
      Transition pre post
  | exhaust {pre post : RequestContext} :
      pre.state = .failed →
      (pre.retriesExhausted ∨ pre.deadlineExceeded) →
      post = { pre with state := .dead, admission := .released } →
      Transition pre post
  | deadline_expire {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      pre.deadlineExceeded →
      post = { pre with state := .dead, admission := .released } →
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
  | interrupt_input_required {pre post : RequestContext} :
      pre.state = .inputRequired →
      pre.admission = .executing →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      Transition pre post


end RequestContext
