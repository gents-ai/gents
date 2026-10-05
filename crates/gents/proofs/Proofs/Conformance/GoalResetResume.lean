import Proofs.GoalAutomation.ResetResume
import Proofs.Conformance.GoalOperatorResume

/-! #2121: reset resume cases replayed against the Rust owner. Times are
seconds after 2030-01-01T00:00:00Z. Every case commits; a case that is not due
or not usage-limited publishes nothing. -/
namespace Conformance.GoalResetResumeContracts
open GoalAutomation.OperatorResume
open Conformance.GoalOperatorResumeContracts (parentBinding snapshotJson requestJson)

def limited : Snapshot :=
  ⟨⟨.usageLimited, 0, false, false⟩, 0, none, 10, [], 37, some 1000⟩
def request : Request := ⟨.usageLimited, 0, true, true, true, true, parentBinding⟩
/-- Explicit durable expectation, never computed from `resumeBy`. -/
def resumed : Snapshot :=
  ⟨⟨.active, 0, false, false⟩, 1, some 10, 20, [parentBinding], 37, some 1000⟩
/-- Limited call started at 0, reset reported for 3600, read at 3600. -/
def due : ResetFacts := ⟨true, some 3600, 0, 3600, true, true, true⟩
def paused : Snapshot := { limited with goal := { limited.goal with status := .paused } }

/-- The first child (request 20) stopped on a second limit, started at 3600. -/
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

private def b (x : Bool) : String := if x then "true" else "false"
private def outcomeJson : Outcome → String
  | .denied => "denied" | .stale => "stale" | .illegal => "illegal"
  | .conflict => "conflict" | .rolledBack => "rolled_back"
  | .created => "created" | .recovered => "recovered"
  | .deferred => "deferred" | .invalidEvidence => "invalid_evidence"
  | .unavailable => "unavailable"

def factsJson (f : ResetFacts) : String :=
  "{\"opted_in\":" ++ b f.optedIn ++ ",\"reset_at\":" ++
    (match f.resetAt with | none => "null" | some t => toString t) ++
  ",\"limit_started_at\":" ++ toString f.limitStartedAt ++ ",\"now\":" ++ toString f.now ++
  ",\"profile_names_account\":" ++ b f.profileNamesAccount ++
  ",\"account_enabled\":" ++ b f.accountEnabled ++
  ",\"backend_enabled\":" ++ b f.backendEnabled ++ "}"

def caseJson (c : ResetCase) : String :=
  "{\"name\":" ++ Conformance.Contracts.jsonString c.name ++
  ",\"before\":" ++ snapshotJson c.before ++ ",\"facts\":" ++ factsJson c.facts ++
  ",\"request\":" ++ requestJson c.request ++ ",\"commit\":" ++ b c.commit ++
  ",\"expected\":" ++ snapshotJson c.expected ++
  ",\"outcome\":" ++ Conformance.Contracts.jsonString (outcomeJson c.outcome) ++ "}"

def casesJson : String := Conformance.Contracts.jsonArray (resetCases.map caseJson)

end Conformance.GoalResetResumeContracts
