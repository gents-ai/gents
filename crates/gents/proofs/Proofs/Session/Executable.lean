import Proofs.Session.Transition

namespace SessionQueue

inductive Action where
  | appendPending (entry : QueueEntry)
  | coalescePending (entry : QueueEntry)
  | claimNext
  | finishActive
  | drainAutomated (source : QueueSource) (queueKey : Option QueueKey)
  deriving DecidableEq, Repr

def step? (pre : SessionQueueState) : Action → Option SessionQueueState
  | .appendPending entry =>
      if entry.policy = .append ∧
          entry.appendWellFormed ∧
          RequestIdFresh pre entry ∧
          canAppendAfter pre.pending entry = true then
        some (pre.appendPending entry)
      else
        none
  | .coalescePending entry =>
      match entry.queueKey with
      | none => none
      | some key =>
          if entry.coalesceWellFormed key then
            if containsCoalescedQueueKey pre.pending entry.source key = true then
              some pre
            else if RequestIdFresh pre entry ∧ canAppendAfter pre.pending entry = true then
              some (pre.appendPending entry)
            else
              none
          else
            none
  | .claimNext =>
      match pre.active, pre.pending with
      | none, entry :: rest => some (pre.claimHead entry rest)
      | _, _ => none
  | .finishActive =>
      match pre.active with
      | some requestId => some (pre.finishActive requestId)
      | none => none
  | .drainAutomated source queueKey =>
      if source.automatedWakeup then
        some (pre.drainAutomatedWakeups source queueKey)
      else
        none

/-- Existing queue step behind its database selection boundary. A missing requester
is an exact scope value, never a wildcard over requester-owned queues. -/
def scopedStep? (target : AgentSession.Scope) (pre : SessionQueueState)
    (action : Action) : Option SessionQueueState :=
  if pre.scope = target then step? pre action else none

theorem foreign_owner_cannot_drain (target : AgentSession.Scope)
    (pre : SessionQueueState) (h : pre.scope.agent ≠ target.agent)
    (source : QueueSource) (key : Option QueueKey) :
    scopedStep? target pre (.drainAutomated source key) = none := by
  have hs : pre.scope ≠ target := by intro he; exact h (congrArg AgentSession.Scope.agent he)
  simp [scopedStep?, hs]

theorem foreign_requester_cannot_drain (target : AgentSession.Scope)
    (pre : SessionQueueState) (h : pre.scope.requester ≠ target.requester)
    (source : QueueSource) (key : Option QueueKey) :
    scopedStep? target pre (.drainAutomated source key) = none := by
  have hs : pre.scope ≠ target := by intro he; exact h (congrArg AgentSession.Scope.requester he)
  simp [scopedStep?, hs]

theorem exact_scope_preserves_queue_step (pre : SessionQueueState) (action : Action) :
    scopedStep? pre.scope pre action = step? pre action := by simp [scopedStep?]

def replay? : SessionQueueState → List Action → Option SessionQueueState
  | s, [] => some s
  | s, action :: rest =>
      match step? s action with
      | some s' => replay? s' rest
      | none => none

end SessionQueue
