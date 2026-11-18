import Proofs.Automation.GoalAutomation.ClientStart
import Proofs.Conformance.Contracts.Json.SessionDocuments
import Proofs.Conformance.GoalClientControl
import Proofs.Conformance.Family

namespace Conformance.GoalClientStartContracts

open GoalAutomation.ClientStart Conformance.Contracts Lean

def requested : AgentSession.Document :=
  { scope := ⟨1, 10, some 1⟩, agent := 3, createdAt := 5 }
def empty : State := ⟨some requested, ⟨false, false, false, false⟩⟩
def facts : Facts := ⟨⟨true, true, false, false⟩, false, 2, 10, true, false,
  some "session-document", some "session-document", none, some "initial-assignment", none,
  3, true, true, 0, 0, true, 0, 0⟩

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
  [ { name := "reserved_session" }
  , { name := "unreserved_session", before := { empty with session := none },
      facts := { facts with currentSessionDocument := none, expectedSessionDocument := none } }
  , { name := "wrong_signed_agent", facts := { facts with selectedAgent := 4 } }
  , { name := "requester_revoked", facts := { facts with requesterAuthorized := false } }
  , { name := "concurrent_revocation", facts := { facts with committedAuthorityVersion := 1 } }
  , { name := "invocation_revoked", facts := { facts with requesterMayInvoke := false } }
  , { name := "concurrent_invocation_revocation", facts := { facts with committedInvokeVersion := 1 } }
  , { name := "authority_unavailable", facts := { facts with authorityAvailable := false } }
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

structure ClaimWire where
  owner : String
  session : String
  objective : String
  tokenBudget : Option Int
  deriving ToJson

def ClaimWire.ofModel (c : GoalAutomation.Fingerprint) : ClaimWire :=
  ⟨c.owner, c.session, c.objective, c.tokenBudget⟩

def ClaimWire.toModel (c : ClaimWire) : GoalAutomation.Fingerprint :=
  ⟨c.owner, c.session, c.objective, c.tokenBudget⟩

structure StartInput where
  name : String
  existingSession : Option DocumentWire
  requested : DocumentWire
  existingGoal : Bool
  objective : String
  budget : Option Int
  signatureValid : Bool
  sessionOwned : Bool
  enrollmentFresh : Bool
  routeApplied : Bool
  hasPeer : Bool
  now : Nat
  expiresAt : Nat
  firstRequestValid : Bool
  busy : Bool
  currentSessionDocument : Option String
  expectedSessionDocument : Option String
  claim : Option ClaimWire
  firstGoal : Option String
  firstRoot : Option String
  selectedAgent : Nat
  requesterAuthorized : Bool
  authorityAvailable : Bool
  authorityVersion : Nat
  committedAuthorityVersion : Nat
  requesterMayInvoke : Bool
  invokeVersion : Nat
  committedInvokeVersion : Nat
  commit : Bool
  deriving ToJson

def StartInput.ofCase (c : Case) : StartInput :=
  { name := c.name, existingSession := c.before.session.map DocumentWire.ofModel
    requested := DocumentWire.ofModel GoalClientStartContracts.requested, existingGoal := c.before.submission.durableGoal
    objective := c.objective, budget := c.budget
    signatureValid := c.facts.authority.signatureValid, sessionOwned := c.facts.authority.sessionOwned
    enrollmentFresh := c.facts.authority.enrollmentFresh, routeApplied := c.facts.authority.routeApplied
    hasPeer := c.facts.hasPeer, now := c.facts.now, expiresAt := c.facts.expiresAt
    firstRequestValid := c.facts.firstRequestValid, busy := c.facts.unfinishedRequest
    currentSessionDocument := c.facts.currentSessionDocument
    expectedSessionDocument := c.facts.expectedSessionDocument
    claim := c.facts.existingClaim.map ClaimWire.ofModel
    firstGoal := c.facts.firstGoalId, firstRoot := c.facts.firstRoot
    selectedAgent := c.facts.selectedAgent, requesterAuthorized := c.facts.requesterAuthorized
    authorityAvailable := c.facts.authorityAvailable, authorityVersion := c.facts.authorityVersion
    committedAuthorityVersion := c.facts.committedAuthorityVersion
    requesterMayInvoke := c.facts.requesterMayInvoke, invokeVersion := c.facts.invokeVersion
    committedInvokeVersion := c.facts.committedInvokeVersion, commit := c.commit }

def StartInput.toCase (i : StartInput) : Case :=
  { name := i.name
    before := ⟨i.existingSession.map DocumentWire.toModel, ⟨i.existingGoal, false, false, false⟩⟩
    objective := i.objective, budget := i.budget
    facts :=
      { authority := ⟨i.signatureValid, i.sessionOwned, i.enrollmentFresh, i.routeApplied⟩
        hasPeer := i.hasPeer, now := i.now, expiresAt := i.expiresAt
        firstRequestValid := i.firstRequestValid, unfinishedRequest := i.busy
        currentSessionDocument := i.currentSessionDocument
        expectedSessionDocument := i.expectedSessionDocument
        existingClaim := i.claim.map ClaimWire.toModel
        firstGoalId := i.firstGoal, firstRoot := i.firstRoot, selectedAgent := i.selectedAgent
        requesterAuthorized := i.requesterAuthorized, authorityAvailable := i.authorityAvailable
        authorityVersion := i.authorityVersion
        committedAuthorityVersion := i.committedAuthorityVersion
        requesterMayInvoke := i.requesterMayInvoke, invokeVersion := i.invokeVersion
        committedInvokeVersion := i.committedInvokeVersion }
    commit := i.commit }

example : cases.all (fun c => decide ((StartInput.ofCase c).toCase = c)) := by native_decide

example : (DocumentWire.ofModel requested).toModel = requested := by native_decide

structure StartOutput where
  published : Bool
  deriving ToJson

def startOutput (i : StartInput) : StartOutput :=
  let c := i.toCase
  { published := (start? c.before i.requested.toModel c.objective c.budget c.facts c.commit).isSome }

@[conformance_family]
def starts : Family StartInput StartOutput where
  inputs := cases.map StartInput.ofCase
  model := startOutput

structure AssignmentWire where
  id : String
  creationKey : String
  snapshot : GoalClientControlContracts.GoalSnapshot
  rootRequest : String
  usageBaseline : Nat
  activeSeconds : Nat
  deriving ToJson

def AssignmentWire.ofModel (a : Assignment) : AssignmentWire :=
  ⟨a.id, a.creationKey, GoalClientControlContracts.GoalSnapshot.ofModel a.snapshot, a.rootRequest,
    a.usageBaseline, a.activeSeconds⟩

def AssignmentWire.toModel (a : AssignmentWire) : Assignment :=
  ⟨a.id, a.creationKey, a.snapshot.toModel, a.rootRequest, a.usageBaseline, a.activeSeconds⟩

structure AssignmentsWire where
  current : AssignmentWire
  retained : List AssignmentWire
  deriving ToJson

def AssignmentsWire.ofModel (a : Assignments) : AssignmentsWire :=
  ⟨AssignmentWire.ofModel a.current, a.retained.map AssignmentWire.ofModel⟩

def AssignmentsWire.toModel (a : AssignmentsWire) : Assignments :=
  ⟨a.current.toModel, a.retained.map AssignmentWire.toModel⟩

structure ReplacementInput where
  name : String
  start : StartInput
  before : AssignmentsWire
  expectedRevision : GoalAutomation.ClientControl.Revision
  nextId : String
  creationKey : String
  firstRequest : String
  nextDocument : String
  nextComposite : String
  sessionUsage : Nat
  deriving ToJson

def ReplacementInput.ofCase (c : ReplacementCase) : ReplacementInput :=
  let replacement := replacementInput c
  { name := c.name
    start := StartInput.ofCase
      { name := c.name, before := ⟨some existing, ⟨true, false, false, false⟩⟩
        objective := c.objective, budget := c.budget, facts := c.facts, commit := c.commit }
    before := AssignmentsWire.ofModel (replacementAssignments c)
    expectedRevision := replacement.expected, nextId := replacement.nextId
    creationKey := replacement.creationKey, firstRequest := replacement.firstRequest
    nextDocument := replacement.nextRevision.document
    nextComposite := replacement.nextRevision.composite
    sessionUsage := replacement.sessionUsage }

example : replacementCases.all (fun c =>
    decide ((ReplacementInput.ofCase c).before.toModel = replacementAssignments c)) := by
  native_decide

structure ReplacedWire where
  status : Goals.Status
  objective : String
  tokensUsed : Nat
  tokenBudget : Option Nat
  sequence : Nat
  usageBaseline : Nat
  activeSeconds : Nat
  deriving ToJson

structure ReplacementOutput where
  expected : Option ReplacedWire
  deriving ToJson

def replacementOutput (i : ReplacementInput) : ReplacementOutput :=
  let c := i.start.toCase
  let replacement : Replacement :=
    { expected := i.expectedRevision, nextId := i.nextId, creationKey := i.creationKey
      nextRevision := ⟨i.nextDocument, i.nextComposite⟩, firstRequest := i.firstRequest
      sessionUsage := i.sessionUsage }
  { expected := (replace? c.before i.before.toModel replacement i.start.requested.toModel
      c.objective c.budget c.facts c.commit).map fun post =>
        let current := post.2.current
        { status := current.snapshot.execution.goal.status
          objective := current.snapshot.objective
          tokensUsed := current.snapshot.execution.tokensUsed
          tokenBudget := current.snapshot.execution.tokenBudget
          sequence := current.snapshot.execution.sequence
          usageBaseline := current.usageBaseline
          activeSeconds := current.activeSeconds } }

@[conformance_family]
def replacements : Family ReplacementInput ReplacementOutput where
  inputs := replacementCases.map ReplacementInput.ofCase
  model := replacementOutput

structure ReservationInput where
  requested : DocumentWire
  user : Nat
  physical : Nat
  retryPhysical : Nat
  agentPhysical : Nat
  deriving ToJson

structure ReservationOutput where
  userOwned : Bool
  agentGranted : Bool
  retryReusesSession : Bool
  agentIsOwner : Bool
  deriving ToJson

@[conformance_family]
def reservation : Family ReservationInput ReservationOutput where
  inputs := [{ requested := DocumentWire.ofModel requested, user := 1, physical := 8
               retryPhysical := 99, agentPhysical := 12 }]
  model := fun i =>
    let requested := i.requested.toModel
    let reserved := (reserve? {} requested i.user i.physical true).getD {}
    let agentRequest := { requested with scope.requester := some requested.agent }
    let agentReserved := (reserve? {} agentRequest requested.agent i.agentPhysical true).getD {}
    { userOwned := reserved.owner == some i.user
      agentGranted := reserved.worker == some requested.agent
      retryReusesSession := decide (reserve? reserved requested i.user i.retryPhysical true = some reserved)
      agentIsOwner := agentReserved.owner == some requested.agent }

structure OpeningProvenanceInput where
  sessionRequester : Nat
  delegate : Nat
  deriving ToJson

structure OpeningProvenanceOutput where
  delegatedAuthor : Bool
  substitutedRequester : Bool
  deriving ToJson

@[conformance_family]
def openingProvenance : Family OpeningProvenanceInput OpeningProvenanceOutput where
  inputs := [{ sessionRequester := 1, delegate := 2 }]
  model := fun i =>
    { delegatedAuthor :=
        openingProvenanceValid i.sessionRequester i.sessionRequester i.delegate i.delegate
      substitutedRequester :=
        openingProvenanceValid i.sessionRequester i.delegate i.delegate i.delegate }

example : replacementCases.length = 24 := by native_decide

example : (cases[1]!).before.session = (start? (cases[1]!).before requested "ship" (some 1000)
    boundFacts true).bind (·.session) := by native_decide

end Conformance.GoalClientStartContracts
