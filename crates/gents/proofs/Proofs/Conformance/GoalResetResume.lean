import Proofs.Automation.GoalAutomation.ResetResume
import Proofs.Conformance.GoalOperatorResume
import Proofs.Conformance.GoalRequestAssignment
import Proofs.Conformance.Family

/-! Times are seconds after 2030-01-01T00:00:00Z. -/
namespace Conformance.GoalResetResumeContracts
open GoalAutomation.OperatorResume
open Conformance.GoalOperatorResumeContracts (parentBinding)
open Lean

def limited : Snapshot :=
  ⟨⟨.usageLimited, 0, false, false⟩, 0, none, 10, [], 37, some 1000⟩
def request : Request := ⟨.usageLimited, 0, true, true, true, true, parentBinding⟩
def resumed : Snapshot :=
  ⟨⟨.active, 0, false, false⟩, 1, some 10, 20, [parentBinding], 37, some 1000⟩
def due : ResetFacts := ⟨true, some 3600, 0, 3600, true, true, true⟩
def paused : Snapshot := { limited with goal := { limited.goal with status := .paused } }

def limitedAgain : Snapshot :=
  { resumed with goal := { resumed.goal with status := .usageLimited } }
def childBinding : Binding :=
  { parentBinding with predecessor := 20, predecessorDoc := 200, child := 40, sequence := 2 }
def requestAgain : Request := ⟨.usageLimited, 1, true, true, true, true, childBinding⟩
def resumedAgain : Snapshot :=
  ⟨⟨.active, 0, false, false⟩, 2, some 20, 40, [childBinding, parentBinding], 37, some 1000⟩

structure ResetCase where
  name : String
  before : Snapshot
  facts : ResetFacts
  request : Request
  commit : Bool
  expected : Snapshot
  outcome : Outcome
  deriving DecidableEq, Repr

def resetCases : List ResetCase :=
  [ ⟨"reset_reached_resumes", limited, due, request, true, resumed, .created⟩
  , ⟨"retry_after_reset_resume_is_noop", resumed, due, request, true, resumed, .illegal⟩
  , ⟨"not_opted_in_waits", limited, {due with optedIn := false}, request, true, limited, .deferred⟩
  , ⟨"reset_not_reported_waits", limited, {due with resetAt := none}, request, true, limited, .deferred⟩
  , ⟨"before_reset_waits", limited, {due with now := 3599}, request, true, limited, .deferred⟩
  , ⟨"profile_moved_waits", limited, {due with profileNamesAccount := false}, request, true,
       limited, .deferred⟩
  , ⟨"account_disabled_waits", limited, {due with accountEnabled := false}, request, true,
       limited, .deferred⟩
  , ⟨"backend_disabled_waits", limited, {due with backendEnabled := false}, request, true,
       limited, .deferred⟩
  , ⟨"stale_reset_waits", limited, {due with limitStartedAt := 7200, now := 7300}, request, true,
       limited, .deferred⟩
  , ⟨"paused_goal_is_not_timer_resumed", paused, due, {request with expectedStatus := .paused},
       true, paused, .illegal⟩
  , ⟨"repeat_limit_resumes_at_its_new_reset", limitedAgain,
       ⟨true, some 7200, 3600, 7200, true, true, true⟩, requestAgain, true, resumedAgain, .created⟩
  ]

theorem cases_replay_explicit_expectations :
    ∀ c ∈ resetCases,
      resumeBy c.before (.resetReached c.facts) c.request c.commit = (c.expected, c.outcome) := by
  decide

deriving instance ToJson for GoalAutomation.OperatorResume.Binding,
  GoalAutomation.OperatorResume.Outcome, GoalAutomation.OperatorResume.ResetFacts

structure SnapshotWire where
  status : Goals.Status
  blockedAudits : Nat
  wrapupRequested : Bool
  wrapupCompleted : Bool
  sequence : Nat
  lastContinuedFrom : Option Nat
  latestRequest : Nat
  children : List Binding
  tokensUsed : Nat
  tokenBudget : Option Nat
  deriving ToJson

def SnapshotWire.ofModel (s : Snapshot) : SnapshotWire :=
  ⟨s.goal.status, s.goal.blockedAudits, s.goal.wrapupRequested, s.goal.wrapupCompleted,
    s.sequence, s.lastContinuedFrom, s.latestRequest, s.children, s.tokensUsed, s.tokenBudget⟩

def SnapshotWire.toModel (s : SnapshotWire) : Snapshot :=
  ⟨⟨s.status, s.blockedAudits, s.wrapupRequested, s.wrapupCompleted⟩,
    s.sequence, s.lastContinuedFrom, s.latestRequest, s.children, s.tokensUsed, s.tokenBudget⟩

structure RequestWire where
  expectedStatus : Goals.Status
  expectedSequence : Nat
  authorized : Bool
  parentBelongsToGoal : Bool
  terminalParent : Bool
  sessionIdle : Bool
  binding : Binding
  deriving ToJson

def RequestWire.ofModel (r : Request) : RequestWire :=
  ⟨r.expectedStatus, r.expectedSequence, r.authorized, r.parentBelongsToGoal,
    r.terminalParent, r.sessionIdle, r.binding⟩

def RequestWire.toModel (r : RequestWire) : Request :=
  ⟨r.expectedStatus, r.expectedSequence, r.authorized, r.parentBelongsToGoal,
    r.terminalParent, r.sessionIdle, r.binding⟩

structure ResetInput where
  name : String
  before : SnapshotWire
  facts : ResetFacts
  request : RequestWire
  commit : Bool
  deriving ToJson

structure ResetOutput where
  expected : SnapshotWire
  outcome : Outcome
  deriving ToJson

@[conformance_family]
def reset : Family ResetInput ResetOutput where
  inputs := resetCases.map fun c =>
    { name := c.name, before := SnapshotWire.ofModel c.before, facts := c.facts
      request := RequestWire.ofModel c.request, commit := c.commit }
  model := fun i =>
    let (after, outcome) :=
      resumeBy i.before.toModel (.resetReached i.facts) i.request.toModel i.commit
    { expected := SnapshotWire.ofModel after, outcome }

end Conformance.GoalResetResumeContracts
