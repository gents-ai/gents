import Proofs.Request.State

namespace RequestContext

inductive Transition : RequestContext → RequestContext → Prop where
  | bind_workspace {pre post : RequestContext} :
      pre.state = .workspaceBindingPending →
      pre.admission = .released →
      post = { pre with state := .pending } →
      Transition pre post
  | claim {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
      pre.ttlOpen →
      post = { pre with state := .claimed, admission := .waiting, claimTime := pre.currentTime, deadline := pre.claimDeadline } →
      Transition pre post
  | dedup_lose {pre post : RequestContext} :
      (pre.state = .pending ∨ pre.state = .workspaceBindingPending) →
      pre.admission = .released →
      post = { pre with state := .superseded } →
      Transition pre post
  | admission_reject {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
      post = { pre with state := .failed, admission := .released } →
      Transition pre post
  | begin_inference {pre post : RequestContext} :
      pre.state = .claimed →
      pre.admission = .acquired →
      post = { pre with state := .processing, admission := .executing } →
      Transition pre post
  /-- Owned-loop processing stutter, not evidence of work, output progress, fairness, or a request-row write. -/
  | continue_processing {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      post = pre →
      Transition pre post
  /-- The canonical output owner must durably close the active attempt and
  preserve its effects before its execution slot can be released. -/
  | park_backend {pre post : RequestContext} {closed : Bool} :
      pre.state = .processing →
      pre.admission = .executing →
      closed = true →
      pre.interruptRequestedAt = none →
      pre.currentTime < pre.deadline →
      post = { pre with state := .waitingForBackend, admission := .released,
                         backendAttempts := pre.backendAttempts + 1,
                         backendRetryAt := RequestBackendWake.fallback pre.currentTime pre.deadline pre.backendAttempts } →
      Transition pre post
  /-- A health observation or bounded fallback may reclaim the parked request;
  the original deadline is preserved rather than synthesised again. -/
  | resume_backend {pre post : RequestContext} {wake : Bool} :
      pre.state = .waitingForBackend →
      pre.admission = .released →
      (wake = true ∨ pre.backendDue) →
      pre.interruptRequestedAt = none →
      pre.currentTime < pre.deadline →
      post = { pre with state := .resumingBackend, admission := .waiting } →
      Transition pre post
  | begin_resumed {pre post : RequestContext} :
      pre.state = .resumingBackend →
      pre.admission = .acquired →
      pre.interruptRequestedAt = none →
      pre.currentTime < pre.deadline →
      post = { pre with state := .processing, admission := .executing } →
      Transition pre post
  | interrupt_resuming {pre post : RequestContext} :
      pre.state = .resumingBackend →
      (pre.admission = .waiting ∨ pre.admission = .acquired) →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      Transition pre post
  | expire_resuming {pre post : RequestContext} :
      pre.state = .resumingBackend →
      (pre.admission = .waiting ∨ pre.admission = .acquired) →
      pre.interruptRequestedAt = none →
      pre.deadline ≤ pre.currentTime →
      post = { pre with state := .failed, admission := .released } →
      Transition pre post
  | interrupt_backend_wait {pre post : RequestContext} :
      pre.state = .waitingForBackend →
      pre.admission = .released →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted } →
      Transition pre post
  | expire_backend_wait {pre post : RequestContext} :
      pre.state = .waitingForBackend →
      pre.admission = .released →
      pre.interruptRequestedAt = none →
      pre.deadline ≤ pre.currentTime →
      post = { pre with state := .failed } →
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
