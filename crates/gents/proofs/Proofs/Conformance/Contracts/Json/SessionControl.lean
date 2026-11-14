import Proofs.Automation.Session.Control
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.SessionControlContracts

open Conformance.Contracts SessionQueue.Control

def boolJson (value : Bool) : String := if value then "true" else "false"

def statusJson (status : Goals.Status) : String :=
  jsonString status.toDefraDB

def settledStopped : State :=
  { queuedStopPost with request := none }
def resumePost : State := (step? scope settledStopped .resume).getD settledStopped
def resumeRefusedWhileActive : Bool :=
  (step? scope ((step? scope initial .stop).getD initial) .resume).isNone
def terminalStopped : State :=
  { queuedStopPost with goal := { queuedStopPost.goal with status := .complete } }
def terminalResumePost : State :=
  (step? scope terminalStopped .resume).getD terminalStopped
def terminalRunningResumeBlocked : Bool :=
  (step? scope terminalParentWithRunningBackground .resume).isNone
def terminalRunningSettledResume : State :=
  (step? scope
    ((step? scope terminalParentWithRunningBackground .settleOwnedBackground).getD
      terminalParentWithRunningBackground) .resume).getD terminalParentWithRunningBackground

structure PendingInput where
  name : String
  initialPendingCount : Nat
  deriving Lean.ToJson

structure NamedInput where
  name : String
  deriving Lean.ToJson

structure GoalOutput where
  goalStatus : String
  stopped : Bool
  deriving Lean.ToJson

structure DrainOutput where
  goalStatus : String
  stopped : Bool
  pendingEmpty : Bool
  deriving Lean.ToJson

structure ResumeOutput where
  goalStatus : String
  stopped : Bool
  refusedWhileActive : Bool
  deriving Lean.ToJson

structure OwnedBackgroundOutput where
  blocked : Bool
  stoppedAfterSettlement : Bool
  deriving Lean.ToJson

@[conformance_family]
def interruptIdle : Conformance.Family PendingInput GoalOutput where
  inputs := [⟨"interrupt_idle", idleInterruptInitial.queue.pending.length⟩]
  model := fun _ => ⟨idleInterruptPost.goal.status.toDefraDB, idleInterruptPost.stopped⟩

@[conformance_family]
def stopDrains : Conformance.Family PendingInput DrainOutput where
  inputs := [⟨"stop_drains", queuedStopInitial.queue.pending.length⟩]
  model := fun _ =>
    ⟨queuedStopPost.goal.status.toDefraDB, queuedStopPost.stopped,
      queuedStopPost.queue.pending.isEmpty⟩

@[conformance_family]
def resumeTerminalGoal : Conformance.Family NamedInput GoalOutput where
  inputs := [⟨"resume_terminal_goal"⟩]
  model := fun _ => ⟨terminalResumePost.goal.status.toDefraDB, terminalResumePost.stopped⟩

@[conformance_family]
def resumeAfterSettlement : Conformance.Family NamedInput ResumeOutput where
  inputs := [⟨"resume_after_settlement"⟩]
  model := fun _ =>
    ⟨resumePost.goal.status.toDefraDB, resumePost.stopped, resumeRefusedWhileActive⟩

/-- Replayed by the registered background owner's stop scenario, whose live
worker holds the process until the owner's reconcile stops it. -/
@[conformance_family]
def resumeWaitsForLiveWorker : Conformance.Family NamedInput OwnedBackgroundOutput where
  inputs := [⟨"resume_waits_for_owned_background"⟩]
  model := fun _ => ⟨terminalRunningResumeBlocked, terminalRunningSettledResume.stopped⟩

/-- Replayed by the scenario whose tool row is already terminal while its
process record is still live. -/
@[conformance_family]
def resumeWaitsForProcessRecord : Conformance.Family NamedInput OwnedBackgroundOutput where
  inputs := [⟨"resume_waits_for_owned_background"⟩]
  model := fun _ => ⟨terminalRunningResumeBlocked, terminalRunningSettledResume.stopped⟩

end Conformance.SessionControlContracts
