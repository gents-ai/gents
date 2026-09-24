import Proofs.CanonicalOutput.Execution.GoalContinuation
import Proofs.CanonicalOutput.Execution.Examples

namespace CanonicalOutput.Execution.GoalContinuation.Cases

open GoalAutomation.OperatorResume
open CanonicalOutput.Execution.Examples

def goalBinding : GoalAutomation.OperatorResume.Binding :=
  { goal := 70, owner := "owner", session := "session", predecessor := 10
  , predecessorDoc := 10, correlation := none, sourceDocument := none
  , triggerContext := none, workspaceFingerprint := none
  , semanticFingerprint := "signed-goal-child", child := 20, sequence := 1 }

def claimedGoal : Snapshot :=
  { goal := ⟨.active, 0, false, false⟩, sequence := 1
  , lastContinuedFrom := some 10, latestRequest := 10, children := []
  , tokensUsed := 0, tokenBudget := some 100 }

def claimedRequest : ClaimedRequest :=
  { expectedStatus := .active, expectedSequence := 1, authorized := true
  , parentBelongsToGoal := true, terminalParent := true, sessionIdle := true
  , binding := goalBinding, expectedLastContinuedFrom := some 10 }

def goalEntry : SessionQueue.QueueEntry :=
  { requestId := 20, createdAt := 5, source := .goal, policy := .coalesce
  , queueKey := some 1, queuedAfter := some 10 }

def physicalBinding (status : Goals.Status := .active) : GoalContinuation.Binding :=
  { goalDocument := 70, goalOwner := "owner", goalSession := "session"
  , observedStatus := status, executionAgent := 1, executionSession := 1
  , parentDocument := 10, parentLogical := 10, childDocument := 200, childRequester := none
  , childEntry := goalEntry, authenticated := true }

def parentLease : RequestExecutionLease.World Generation :=
  { request := .completed, lease := .terminal 7 .completed, usedGenerations := [7]
  , now := 5, continuationRequired := false, tokenChargeRequired := false
  , continuationCount := 0, tokenChargeCount := 0 }

def parentWorld : World :=
  { requestId := 10, sessionId := 1, purpose := .normal, principal := 1, remoteRoutes := []
  , lease := parentLease, segments := [], messages := [], transcript := transcript
  , compactionCursor := none, toolContexts := [], delegatedCalls := []
  , terminalSelection := some .noMessage }

def idleQueue : SessionQueue.SessionQueueState :=
  { scope := ⟨1, 1, none⟩, active := none, pending := [], terminal := ∅ }

def heldState : Option World := do
  let gate ← Gate.acquire (Gate.initial parentWorld) 1 true
  pure { gate with queue := idleQueue, claimed := none }

def reacquire (state : World) : Option World := do
  let released ← Gate.scheduling state 1 .release
  let held ← Gate.acquire released 1 true
  pure held

def freshReplayAndClaim : Option Bool := do
  let state ← heldState
  let fresh ← publishGoalChild? claimedGoal state 1 5 claimedRequest
    (physicalBinding .active) goalEntry
  if fresh.outcome != .created then none
  let replayState ← reacquire fresh.after
  let replay ← publishGoalChild? fresh.afterGoal replayState 1 5 claimedRequest
    (physicalBinding .active) goalEntry
  if replay.outcome != .recovered ||
      replay.after.queue.active != replay.before.queue.active ||
      replay.after.queue.pending != replay.before.queue.pending ||
      replay.after.queue.terminal != replay.before.queue.terminal
    then none
  let activated ← Handover.claimAndActivate replay.after 1 5
    (childActivation replay [] true 8 5 10)
  pure (activated.requestId == 200 &&
    activated.queue.active == some 20 &&
    activated.claimed.map (·.logicalRequest) == some 20)

theorem fresh_goal_child_replays_then_uses_actual_handover :
    freshReplayAndClaim = some true := by native_decide

def pausedReplayAndStaleFresh : Option Bool := do
  let state ← heldState
  let fresh ← publishGoalChild? claimedGoal state 1 5 claimedRequest
    (physicalBinding .active) goalEntry
  let replayState ← reacquire fresh.after
  let paused := { fresh.afterGoal with goal := { fresh.afterGoal.goal with status := .paused } }
  let pausedRequest := { claimedRequest with expectedStatus := .paused }
  let replay ← publishGoalChild? paused replayState 1 5 pausedRequest
    (physicalBinding .paused) goalEntry
  let otherState ← heldState
  pure (replay.outcome == .recovered &&
    (publishGoalChild? { claimedGoal with goal := { claimedGoal.goal with status := .paused } }
      otherState 1 5 claimedRequest (physicalBinding .paused) goalEntry).isNone &&
    (publishGoalChild? claimedGoal otherState 1 5 claimedRequest
      { physicalBinding .active with parentDocument := 999 } goalEntry).isNone &&
    (publishGoalChild? claimedGoal otherState 1 5 claimedRequest
      { physicalBinding .active with childRequester := some 99 } goalEntry).isNone)

theorem paused_receipt_replays_but_stale_status_and_parent_cannot_publish :
    pausedReplayAndStaleFresh = some true := by native_decide

end CanonicalOutput.Execution.GoalContinuation.Cases
