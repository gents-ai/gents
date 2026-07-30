import Proofs.Session.State

namespace SessionQueue

inductive Transition : SessionQueueState → SessionQueueState → Prop where
  | append_pending {pre post : SessionQueueState} {entry : QueueEntry} :
      entry.policy = .append →
      entry.appendWellFormed →
      RequestIdFresh pre entry →
      canAppendAfter pre.pending entry = true →
      post = pre.appendPending entry →
      Transition pre post
  | coalesce_pending_new {pre post : SessionQueueState} {entry : QueueEntry} {key : QueueKey} :
      entry.coalesceWellFormed key →
      RequestIdFresh pre entry →
      containsCoalescedQueueKey pre.pending entry.source key = false →
      canAppendAfter pre.pending entry = true →
      post = pre.appendPending entry →
      Transition pre post
  | coalesce_pending_existing {pre post : SessionQueueState} {entry : QueueEntry} {key : QueueKey} :
      entry.coalesceWellFormed key →
      containsCoalescedQueueKey pre.pending entry.source key = true →
      post = pre →
      Transition pre post
  | claim_next {pre post : SessionQueueState} {entry : QueueEntry} {rest : List QueueEntry} :
      pre.active = none →
      pre.pending = entry :: rest →
      post = pre.claimHead entry rest →
      Transition pre post
  | finish_active {pre post : SessionQueueState} {requestId : RequestId} :
      pre.active = some requestId →
      post = pre.finishActive requestId →
      Transition pre post
  | drain_automated {pre post : SessionQueueState}
      {source : QueueSource} {queueKey : Option QueueKey} :
      source.automatedWakeup →
      post = pre.drainAutomatedWakeups source queueKey →
      Transition pre post

inductive Trace : SessionQueueState → SessionQueueState → Prop where
  | refl {s : SessionQueueState} : Trace s s
  | step {s₁ s₂ s₃ : SessionQueueState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

end SessionQueue
