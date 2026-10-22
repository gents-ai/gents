import Proofs.Session.Control
import Proofs.Conformance.Contracts.Json.Helpers

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

def casesJson : String :=
  "[" ++
  "{\"name\":\"interrupt_idle\",\"initial_pending_count\":" ++ toString idleInterruptInitial.queue.pending.length ++ ",\"goal_status\":" ++ statusJson idleInterruptPost.goal.status ++
  ",\"stopped\":" ++ boolJson idleInterruptPost.stopped ++ "}," ++
  "{\"name\":\"stop_drains\",\"initial_pending_count\":" ++ toString queuedStopInitial.queue.pending.length ++ ",\"goal_status\":" ++ statusJson queuedStopPost.goal.status ++
  ",\"stopped\":" ++ boolJson queuedStopPost.stopped ++
  ",\"pending_empty\":" ++ boolJson queuedStopPost.queue.pending.isEmpty ++ "}," ++
  "{\"name\":\"resume_after_settlement\",\"goal_status\":" ++ statusJson resumePost.goal.status ++
  ",\"stopped\":" ++ boolJson resumePost.stopped ++
  ",\"refused_while_active\":" ++ boolJson resumeRefusedWhileActive ++ "}," ++
  "{\"name\":\"resume_terminal_goal\",\"goal_status\":" ++ statusJson terminalResumePost.goal.status ++
  ",\"stopped\":" ++ boolJson terminalResumePost.stopped ++ "}," ++
  "{\"name\":\"resume_waits_for_owned_background\",\"blocked\":" ++ boolJson terminalRunningResumeBlocked ++
  ",\"stopped_after_settlement\":" ++ boolJson terminalRunningSettledResume.stopped ++ "}]"

end Conformance.SessionControlContracts
