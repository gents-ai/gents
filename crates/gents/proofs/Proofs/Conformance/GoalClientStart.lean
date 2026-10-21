import Proofs.GoalAutomation.ClientStart
import Proofs.Conformance.Contracts.Json.SessionDocuments
import Proofs.Conformance.GoalClientControl

namespace Conformance.GoalClientStartContracts

open GoalAutomation.ClientStart Conformance.Contracts Lean

def requested : AgentSession.Document :=
  { scope := ⟨1, 10, some 1⟩, agent := 3, createdAt := 5 }
def empty : State := ⟨none, ⟨false, false, false, false⟩⟩
def facts : Facts := ⟨⟨true, true, true, false, false⟩, false, 2, 10, true, false, none, none, none, some "initial-assignment", none⟩

structure Case where
  name : String
  before : State := empty
  objective : String := " ship "
  budget : Option Int := some 1000
  facts : Facts := GoalClientStartContracts.facts
  commit : Bool := true
  deriving DecidableEq, Repr

instance : Inhabited Case := ⟨{ name := "" }⟩

def existing : AgentSession.Document :=
  { requested with
    createdAt := 1
    closedAt := some 4
    title := some ⟨"Preserved", .user⟩
    tags := ["kept"] }
def boundFacts : Facts :=
  { facts with
    currentSessionDocument := some "session-document"
    expectedSessionDocument := some "session-document" }

def cases : List Case :=
  [ { name := "fresh_session" }
  , { name := "reuse_session", before := { empty with session := some existing }, facts := boundFacts }
  , { name := "foreign_agent", before := { empty with session := some { existing with agent := 4 } }, facts := boundFacts }
  , { name := "foreign_requester", before := { empty with session := some { existing with scope.requester := some 9 } },
      facts := { boundFacts with authority.sessionOwned := false } }
  , { name := "invalid_first_request", facts := { facts with firstRequestValid := false } }
  , { name := "invalid_command_signature", facts := { facts with authority.signatureValid := false } }
  , { name := "expired_command", facts := { facts with now := 10 } }
  , { name := "busy_session", facts := { boundFacts with unfinishedRequest := true }, before := { empty with session := some existing } }
  , { name := "recreated_session", facts := { boundFacts with expectedSessionDocument := some "old-header" }, before := { empty with session := some existing } }
  , { name := "unexpected_session", facts := { boundFacts with expectedSessionDocument := none }, before := { empty with session := some existing } }
  , { name := "existing_goal", before := { empty with submission.durableGoal := true } }
  , { name := "blank_objective", objective := "\u3000\u00A0" }
  , { name := "zero_budget", budget := some 0 }
  , { name := "negative_budget", budget := some (-1) }
  , { name := "unbudgeted", budget := none }
  , { name := "maximum_budget", budget := some GoalAutomation.maxTokenBudget }
  , { name := "rollback", commit := false }
  , { name := "matching_creation_claim", facts := { facts with existingClaim := some ⟨"1", "10", "ship", some 1000⟩ } }
  , { name := "missing_first_goal_reference", facts := {facts with firstGoalId := none} }
  , { name := "different_first_goal_reference", facts := {facts with firstGoalId := some "other-assignment"} }
  , { name := "first_request_cannot_claim_existing_root", facts := {facts with firstRoot := some "existing-root"} }
  , { name := "conflicting_creation_claim", facts := { facts with existingClaim := some ⟨"1", "10", "earlier", some 1000⟩ } }
  ]

def sessionJson (session : Option AgentSession.Document) : Json :=
  session.map documentJson |>.getD Json.null

def caseJson (c : Case) : Json :=
  let after := start? c.before requested c.objective c.budget c.facts c.commit
  Json.mkObj [ ("name", toJson c.name), ("session", sessionJson c.before.session),
    ("requested", documentJson requested), ("existing_goal", toJson c.before.submission.durableGoal),
    ("objective", toJson c.objective), ("budget", toJson c.budget),
    ("signature_valid", toJson c.facts.authority.signatureValid),
    ("first_request_valid", toJson c.facts.firstRequestValid),
    ("first_goal", toJson c.facts.firstGoalId), ("first_root", toJson c.facts.firstRoot),
    ("now", toJson c.facts.now), ("expires_at", toJson c.facts.expiresAt),
    ("busy", toJson c.facts.unfinishedRequest),
    ("current_session_document", toJson c.facts.currentSessionDocument),
    ("expected_session_document", toJson c.facts.expectedSessionDocument),
    ("commit", toJson c.commit), ("published", toJson after.isSome),
    ("claim", c.facts.existingClaim.map (fun claim => Json.mkObj
      [("owner", toJson claim.owner), ("session", toJson claim.session),
        ("objective", toJson claim.objective), ("token_budget", toJson claim.tokenBudget)]) |>.getD Json.null),
    ("expected_session", after.map (fun s => sessionJson s.session) |>.getD Json.null) ]

def replacementFacts : Facts := {boundFacts with firstGoalId := some "new-assignment"}

structure ReplacementCase where
  name : String
  status : Goals.Status := .stopped
  revision : GoalAutomation.ClientControl.Revision := ⟨"goal-document", "revision-current"⟩
  nextId : String := "new-assignment"
  creationKey : String := "new-claim"
  firstRequest : String := "new-first-request"
  nextDocument : String := "new-goal-document"
  objective : String := " verify replacement "
  budget : Option Int := some 2000
  facts : Facts := replacementFacts
  retained : Bool := false
  commit : Bool := true
  deriving DecidableEq, Repr

def priorAssignment (status : Goals.Status) : Assignment :=
  { id := "old-assignment", creationKey := "old-claim"
    snapshot := { GoalClientControlContracts.initial status with
      execution.tokensUsed := 317, execution.sequence := 4 }
    rootRequest := "old-first-request", usageBaseline := 91, activeSeconds := 23 }

def replacementAssignments (c : ReplacementCase) : Assignments :=
  { current := priorAssignment c.status
    retained := if c.retained then
      [{ priorAssignment .complete with
          id := "historical-assignment", creationKey := "historical-claim"
          rootRequest := "historical-first-request", snapshot.revision.document := "historical-document" }]
      else [] }

def replacementInput (c : ReplacementCase) : Replacement :=
  { expected := c.revision, nextId := c.nextId, creationKey := c.creationKey
    firstRequest := c.firstRequest, nextRevision := ⟨c.nextDocument, "revision-after"⟩
    sessionUsage := 408 }

def replacementCases : List ReplacementCase :=
  ([Goals.Status.active, .paused, .blocked, .usageLimited, .budgetLimited, .complete, .stopped].map fun status =>
    { name := "replace_" ++ status.toDefraDB, status }) ++
  [ { name := "replace_stale_revision", revision := ⟨"goal-document", "revision-stale"⟩ }
  , { name := "replace_foreign_document", revision := ⟨"foreign-document", "revision-current"⟩ }
  , { name := "replace_busy", facts := { replacementFacts with unfinishedRequest := true } }
  , { name := "replace_invalid_first_request", facts := { replacementFacts with firstRequestValid := false } }
  , { name := "replace_invalid_signature", facts := { replacementFacts with authority.signatureValid := false } }
  , { name := "replace_expired", facts := { replacementFacts with now := 10 } }
  , { name := "replace_wrong_session", facts := { replacementFacts with expectedSessionDocument := some "stale-session" } }
  , { name := "replace_blank", objective := "\u3000\u00A0" }
  , { name := "replace_zero_budget", budget := some 0 }
  , { name := "replace_negative_budget", budget := some (-1) }
  , { name := "replace_unbudgeted", budget := none }
  , { name := "replace_rollback", commit := false }
  , { name := "replace_missing_first_reference", facts := {replacementFacts with firstGoalId := none} }
  , { name := "replace_old_goal_reference", facts := {replacementFacts with firstGoalId := some "old-assignment"} }
  , { name := "replace_old_root_reference", facts := {replacementFacts with firstRoot := some "old-first-request"} }
  , { name := "replace_retains_history", retained := true }
  , { name := "replace_conflicting_claim", facts := { replacementFacts with
      existingClaim := some ⟨"1", "10", "earlier", some 1000⟩ } }
  ]

def assignmentJson (a : Assignment) : Json :=
  Json.mkObj [ ("id", toJson a.id), ("creation_key", toJson a.creationKey),
    ("snapshot", Json.parse (GoalClientControlContracts.snapshotJson a.snapshot) |>.toOption.get!),
    ("root_request", toJson a.rootRequest), ("usage_baseline", toJson a.usageBaseline),
    ("active_seconds", toJson a.activeSeconds) ]

def assignmentsJson (a : Assignments) : Json :=
  Json.mkObj [("current", assignmentJson a.current),
    ("retained", toJson (a.retained.map assignmentJson))]

def replacementCaseJson (c : ReplacementCase) : Json :=
  let before : State := ⟨some existing, ⟨true, false, false, false⟩⟩
  let assignments := replacementAssignments c
  let input := replacementInput c
  let post := replace? before assignments input requested c.objective c.budget c.facts c.commit
  let base := caseJson {
    name := c.name, before, objective := c.objective
    budget := c.budget, facts := c.facts, commit := c.commit }
  Json.mkObj [ ("name", toJson c.name), ("replacement", Json.mkObj [
    ("before", assignmentsJson assignments),
    ("expected_revision", Json.parse (GoalClientControlContracts.revisionJson input.expected) |>.toOption.get!),
    ("next_id", toJson input.nextId), ("creation_key", toJson input.creationKey),
    ("first_request", toJson input.firstRequest), ("next_document", toJson input.nextRevision.document),
    ("session_usage", toJson input.sessionUsage),
    ("start", base),
    ("expected", post.map (fun result => assignmentsJson result.2) |>.getD Json.null) ]) ]

def casesJson : String :=
  (toJson (cases.map caseJson ++ replacementCases.map replacementCaseJson)).compress

example : replacementCases.length = 24 := by native_decide

example : (cases[1]!).before.session = (start? (cases[1]!).before requested "ship" (some 1000)
    boundFacts true).bind (·.session) := by native_decide

end Conformance.GoalClientStartContracts
