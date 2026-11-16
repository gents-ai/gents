import Proofs.Automation.GoalAutomation.ClaimedPublication
import Proofs.Conformance.GoalOperatorResume
import Proofs.Conformance.Family

namespace Conformance.GoalClaimedPublicationContracts
open GoalAutomation.OperatorResume
open Conformance.GoalOperatorResumeContracts (parentBinding SnapshotWire SnapshotWire.ofModel SnapshotWire.toModel)

def claimed : Snapshot :=
  ⟨⟨.active, 2, false, false⟩, 1, some 10, 10, [], 37, some 1000⟩
def request : ClaimedRequest :=
  ⟨⟨.active, 1, true, true, true, true, parentBinding⟩, some 10, some "requester"⟩
def published : Snapshot :=
  ⟨⟨.active, 2, false, false⟩, 1, some 10, 20, [parentBinding], 37, some 1000⟩
def budgetClaimed : Snapshot :=
  { claimed with goal := ⟨.budgetLimited, 2, true, false⟩ }
def paused : Snapshot := { claimed with goal := {claimed.goal with status := .paused} }
def laterEpoch : Snapshot := {claimed with sequence := 3}
def changedWatermark : Snapshot := {claimed with lastContinuedFrom := some 30}
def recoveredLater : Snapshot :=
  {published with goal := {published.goal with status := .paused}, sequence := 3,
                  lastContinuedFrom := some 30, latestRequest := 40}
def noWait : PublicationObservation := ⟨[], some [], false⟩
def runningTool : BackgroundTool :=
  ⟨301, .spawned 300 "300:spawned-background", "spawned:300", "owner", "session",
    some "requester", .running⟩
def timedOutWait : WaitControl :=
  ⟨400, 100, "owner", "session", some "requester", "wait_process",
    "spawned:300", "spawned:300", .timedOutRunning, true, true⟩
def waiting : PublicationObservation := ⟨[timedOutWait], some [runningTool], false⟩
def launchedOnly : PublicationObservation := ⟨[], some [runningTool], false⟩
def completedTool : PublicationObservation :=
  { waiting with backgrounds := some [{runningTool with state := .terminal}] }
def undeliveredTool : PublicationObservation :=
  { waiting with backgrounds := some [{runningTool with state := .completionPending}] }
def pendingTool : PublicationObservation :=
  { waiting with waits := [{timedOutWait with reply := .other}], backgrounds := some [{runningTool with state := .pending}] }
def lostTool : PublicationObservation := { waiting with backgrounds := some [] }
def unreadableTarget : PublicationObservation := { waiting with backgrounds := none }
def ambiguousTarget : PublicationObservation :=
  { waiting with backgrounds := some [runningTool, {runningTool with docId := 302, origin := .nonSpawned, state := .terminal}] }
def malformedTool : BackgroundTool :=
  { runningTool with origin := .spawned 300 "wrong-key" }
def malformedOrigin : PublicationObservation :=
  { waiting with backgrounds := some [malformedTool] }
def unrelatedWait : PublicationObservation :=
  { waiting with waits := [{timedOutWait with parentRequestDoc := 999}] }
def foreignTarget : PublicationObservation :=
  { waiting with backgrounds := some [{runningTool with session := "foreign"}] }
def malformedWait : PublicationObservation :=
  { waiting with waits := [{timedOutWait with replyHandle := "wrong"}] }
def malformedAcceptedWait : PublicationObservation :=
  { waiting with waits := [{timedOutWait with acceptedHandle := "", replyHandle := "", reply := .malformed}] }
def forgedOtherWait : PublicationObservation :=
  { waiting with waits := [{timedOutWait with reply := .malformed}] }
def malformedLater : PublicationObservation :=
  { waiting with waits := [timedOutWait,
      {timedOutWait with docId := 401, replyHandle := "wrong"}] }
def malformedEarlier : PublicationObservation :=
  { waiting with waits := [{timedOutWait with docId := 401, replyHandle := "wrong"},
      timedOutWait] }
def ordinaryToolError : PublicationObservation :=
  { waiting with waits := [{timedOutWait with reply := .argumentError, replyHandle := ""}] }
def callerDeadlineWait : PublicationObservation :=
  { waiting with waits := [{timedOutWait with reply := .other}] }
def cancelledBeforeStart : PublicationObservation :=
  { waiting with waits :=
      [{timedOutWait with reply := WaitResult.settledDiagnostic, replyHandle := "", replied := false}] }
def budgetWrapupAbandoned : Snapshot :=
  { budgetClaimed with goal := {budgetClaimed.goal with wrapupCompleted := true} }

structure PublicationCase where
  name : String
  before : Snapshot
  request : ClaimedRequest
  observation : PublicationObservation
  commit : Bool
  expected : Snapshot
  outcome : Outcome
  deriving DecidableEq, Repr

def cases : List PublicationCase :=
  [ ⟨"claimed_active_publishes_without_reclaim", claimed, request, noWait, true, published, .created⟩
  , ⟨"claimed_budget_wrapup_publishes", budgetClaimed,
       {request with expectedStatus := .budgetLimited}, noWait, true,
       {published with goal := budgetClaimed.goal}, .created⟩
  , ⟨"pause_preempts_unpublished_child", paused, request, noWait, true, paused, .stale⟩
  , ⟨"new_resume_epoch_preempts_old_claim", laterEpoch, request, noWait, true, laterEpoch, .stale⟩
  , ⟨"changed_parent_watermark_preempts", changedWatermark, request, noWait, true, changedWatermark, .stale⟩
  , ⟨"discard_publishes_nothing", claimed, request, noWait, false, claimed, .rolledBack⟩
  , ⟨"exact_receipt_recovers_after_pause_and_progress", recoveredLater, request, noWait, true,
       recoveredLater, .recovered⟩
  , ⟨"foreign_receipt_cannot_recover", {published with children :=
       [{parentBinding with semanticFingerprint := "foreign"}]}, request, noWait, true,
       {published with children := [{parentBinding with semanticFingerprint := "foreign"}]}, .conflict⟩
  , ⟨"claimed_wait_running_defers", claimed, request, waiting, true, claimed, .deferred⟩
  , ⟨"launched_without_wait_publishes", claimed, request, launchedOnly, true, published, .created⟩
  , ⟨"completed_waited_tool_resumes", claimed, request, completedTool, true, published, .created⟩
  , ⟨"completed_waited_tool_with_owed_delivery_defers", claimed, request, undeliveredTool,
       true, claimed, .deferred⟩
  , ⟨"failed_waited_tool_with_owed_delivery_defers", claimed, request, undeliveredTool,
       true, claimed, .deferred⟩
  , ⟨"cancelled_waited_tool_with_owed_delivery_defers", claimed, request, undeliveredTool,
       true, claimed, .deferred⟩
  , ⟨"pending_waited_tool_does_not_suppress", claimed, request, pendingTool, true,
       published, .created⟩
  , ⟨"lost_waited_tool_recovers", claimed, request, lostTool, true, published, .created⟩
  , ⟨"unreadable_target_fails_closed", claimed, request, unreadableTarget, true,
       paused, .invalidEvidence⟩
  , ⟨"ambiguous_same_handle_fails_closed", claimed, request, ambiguousTarget, true,
       paused, .invalidEvidence⟩
  , ⟨"malformed_spawn_origin_fails_closed", claimed, request, malformedOrigin, true,
       paused, .invalidEvidence⟩
  , ⟨"unrelated_wait_cannot_suppress", claimed, request, unrelatedWait, true, published, .created⟩
  , ⟨"foreign_session_target_cannot_suppress", claimed, request, foreignTarget, true, published, .created⟩
  , ⟨"malformed_same_parent_receipt_fails_closed", claimed, request, malformedWait, true,
       paused, .invalidEvidence⟩
  , ⟨"malformed_accepted_wait_error_fails_closed", claimed, request, malformedAcceptedWait, true,
       paused, .invalidEvidence⟩
  , ⟨"forged_nonqualifying_envelope_fails_closed", claimed, request, forgedOtherWait, true,
       paused, .invalidEvidence⟩
  , ⟨"renamed_wait_row_cannot_hide_accepted_control", claimed, request, forgedOtherWait, true,
       paused, .invalidEvidence⟩
  , ⟨"running_then_malformed_wait_fails_closed", claimed, request, malformedLater, true,
       paused, .invalidEvidence⟩
  , ⟨"malformed_then_running_wait_fails_closed", claimed, request, malformedEarlier, true,
       paused, .invalidEvidence⟩
  , ⟨"valid_wait_tool_error_does_not_suppress", claimed, request, ordinaryToolError, true,
       published, .created⟩
  , ⟨"caller_deadline_wait_does_not_suppress", claimed, request, callerDeadlineWait, true,
       published, .created⟩
  , ⟨"bounded_presentation_preserves_raw_wait_receipt", claimed, request,
       ordinaryToolError, true, published, .created⟩
  , ⟨"deadline_settled_wait_does_not_suppress", claimed, request,
       {waiting with waits := [{timedOutWait with reply := .settledDiagnostic}]}, true,
       published, .created⟩
  , ⟨"cancelled_before_start_wait_without_reply_does_not_block", claimed, request,
       cancelledBeforeStart, true, published, .created⟩
  , ⟨"wait_evidence_storage_failure_retries_without_transition", claimed, request,
       {waiting with storageFailed := true}, true, claimed, .unavailable⟩
  , ⟨"abandoned_wrapup_rescan_makes_no_write", budgetWrapupAbandoned,
       {request with expectedStatus := .budgetLimited}, malformedWait, true,
       budgetWrapupAbandoned, .illegal⟩
  , ⟨"abandoned_wrapup_never_publishes_after_evidence_clears", budgetWrapupAbandoned,
       {request with expectedStatus := .budgetLimited}, noWait, true,
       budgetWrapupAbandoned, .illegal⟩
  , ⟨"budget_wrapup_invalid_evidence_abandons_wrapup", budgetClaimed,
       {request with expectedStatus := .budgetLimited}, malformedWait, true,
       budgetWrapupAbandoned, .invalidEvidence⟩
  ]

theorem explicit_cases_replay : ∀ c ∈ cases,
    publishClaimed c.before c.request c.observation c.commit = (c.expected, c.outcome) := by decide

theorem claimed_wait_then_completion_reuses_claim :
    let deferred := publishClaimed claimed request waiting true
    GoalAutomation.continuationStep .unclaimed (.claim true) = .claimed ∧
      deferred = (claimed, .deferred) ∧
      publishClaimed deferred.1 request undeliveredTool true = (claimed, .deferred) ∧
      publishClaimed deferred.1 request completedTool true = (published, .created) := by decide


theorem abandoned_wrapup_claim_stays_inert :
    let budgetRequest := {request with expectedStatus := .budgetLimited}
    publishClaimed budgetClaimed budgetRequest malformedWait true =
        (budgetWrapupAbandoned, .invalidEvidence) ∧
      publishClaimed budgetWrapupAbandoned budgetRequest malformedWait true =
        (budgetWrapupAbandoned, .illegal) ∧
      publishClaimed budgetWrapupAbandoned budgetRequest noWait true =
        (budgetWrapupAbandoned, .illegal) := by decide

deriving instance Lean.ToJson for BackgroundState, WaitResult, WaitControl

structure OriginWire where
  kind : String
  parentToolDoc : Option Nat
  stableKey : Option String
  deriving Lean.ToJson

def OriginWire.ofModel : BackgroundOrigin → OriginWire
  | .spawned parent key => ⟨"spawned", some parent, some key⟩
  | .nonSpawned => ⟨"non_spawned", none, none⟩

def OriginWire.toModel (o : OriginWire) : BackgroundOrigin :=
  match o.kind, o.parentToolDoc, o.stableKey with
  | "spawned", some parent, some key => .spawned parent key
  | _, _, _ => .nonSpawned

structure BackgroundWire where
  docId : Nat
  origin : OriginWire
  handle : String
  owner : String
  session : String
  requester : Option String
  state : BackgroundState
  deriving Lean.ToJson

def BackgroundWire.ofModel (b : BackgroundTool) : BackgroundWire :=
  { docId := b.docId, origin := .ofModel b.origin, handle := b.handle, owner := b.owner
    session := b.session, requester := b.requester, state := b.state }

def BackgroundWire.toModel (b : BackgroundWire) : BackgroundTool :=
  { docId := b.docId, origin := b.origin.toModel, handle := b.handle, owner := b.owner
    session := b.session, requester := b.requester, state := b.state }

structure ObservationWire where
  waits : List WaitControl
  backgrounds : Option (List BackgroundWire)
  storageFailed : Bool
  deriving Lean.ToJson

def ObservationWire.ofModel (o : PublicationObservation) : ObservationWire :=
  { waits := o.waits, backgrounds := o.backgrounds.map (·.map .ofModel)
    storageFailed := o.storageFailed }

def ObservationWire.toModel (o : ObservationWire) : PublicationObservation :=
  { waits := o.waits, backgrounds := o.backgrounds.map (·.map (·.toModel))
    storageFailed := o.storageFailed }

structure ClaimedRequestWire where
  expectedStatus : Goals.Status
  expectedSequence : Nat
  authorized : Bool
  parentBelongsToGoal : Bool
  terminalParent : Bool
  sessionIdle : Bool
  binding : Binding
  expectedLastContinuedFrom : Option Nat
  requester : Option String
  deriving Lean.ToJson

def ClaimedRequestWire.ofModel (r : ClaimedRequest) : ClaimedRequestWire :=
  { expectedStatus := r.expectedStatus, expectedSequence := r.expectedSequence
    authorized := r.authorized, parentBelongsToGoal := r.parentBelongsToGoal
    terminalParent := r.terminalParent, sessionIdle := r.sessionIdle, binding := r.binding
    expectedLastContinuedFrom := r.expectedLastContinuedFrom, requester := r.requester }

def ClaimedRequestWire.toModel (r : ClaimedRequestWire) : ClaimedRequest :=
  { expectedStatus := r.expectedStatus, expectedSequence := r.expectedSequence
    authorized := r.authorized, parentBelongsToGoal := r.parentBelongsToGoal
    terminalParent := r.terminalParent, sessionIdle := r.sessionIdle, binding := r.binding
    expectedLastContinuedFrom := r.expectedLastContinuedFrom, requester := r.requester }

structure PublicationInput where
  name : String
  before : SnapshotWire
  request : ClaimedRequestWire
  observation : ObservationWire
  commit : Bool
  deriving Lean.ToJson

def PublicationInput.ofCase (c : PublicationCase) : PublicationInput :=
  { name := c.name, before := .ofModel c.before, request := .ofModel c.request
    observation := .ofModel c.observation, commit := c.commit }

structure PublicationOutput where
  expected : SnapshotWire
  outcome : Outcome
  deriving Lean.ToJson

def publicationOf (i : PublicationInput) : PublicationOutput :=
  let result := publishClaimed i.before.toModel i.request.toModel i.observation.toModel i.commit
  { expected := .ofModel result.1, outcome := result.2 }

/-- Publications whose parent turn ran no tool: the fixture stages them from a
Goal snapshot and request alone, inside one transaction it commits or
discards. -/
@[conformance_family]
def historical : Family PublicationInput PublicationOutput where
  inputs := (cases.filter fun c =>
    c.observation.waits.isEmpty && c.name != "launched_without_wait_publishes").map .ofCase
  model := publicationOf

/-- The parent turn waits on a background process that is still running when
the publication is attempted. -/
@[conformance_family]
def runningWait : Family PublicationInput PublicationOutput where
  inputs := (cases.filter fun c => c.name == "claimed_wait_running_defers").map .ofCase
  model := publicationOf

/-- The parent turn launched or waited on background tools through the real
tool-call lifecycle before it terminated. -/
@[conformance_family]
def waitObservation : Family PublicationInput PublicationOutput where
  inputs := (cases.filter fun c =>
    c.name == "launched_without_wait_publishes" ||
      (!c.observation.waits.isEmpty && c.name != "claimed_wait_running_defers")).map .ofCase
  model := publicationOf

def identityCases : List (String × String × String × String) :=
  [("agent", "agent", "agent", "agent"),
   ("agent", "node", "agent", "agent"),
   ("agent", "agent", "node", "agent"),
   ("agent", "agent", "agent", "node")]

structure IdentityInput where
  selected : String
  signer : String
  actor : String
  parent : String
  deriving Lean.ToJson

def IdentityInput.ofCase (c : String × String × String × String) : IdentityInput :=
  let (selected, signer, actor, parent) := c
  { selected, signer, actor, parent }

structure CandidateOutput where
  candidateAllowed : Bool
  deriving Lean.ToJson

@[conformance_family]
def identityCandidate : Family IdentityInput CandidateOutput where
  inputs := identityCases.map IdentityInput.ofCase
  model := fun i => { candidateAllowed := goalCandidateInScope i.actor i.selected i.parent }

structure StagedOutput where
  allowed : Bool
  deriving Lean.ToJson

/-- The claimed-publication fixture stages a continuation from its one parent
request, which the selected Agent owns, so only cases whose parent is the
selected identity can be staged. -/
def identityStagedCases : List (String × String × String × String) :=
  identityCases.filter fun (selected, _, _, parent) => parent == selected

@[conformance_family]
def identityStaged : Family IdentityInput StagedOutput where
  inputs := identityStagedCases.map IdentityInput.ofCase
  model := fun i => { allowed := continuationIdentityValid i.selected i.signer i.actor i.parent }

/-- model-production-gap: no production scenario stages a claimed continuation
from a parent request owned by another identity; the goal source never offers
such a parent (`identityCandidate`), and the staging fixture has no foreign
parent request. -/
@[conformance_family]
def identityStagedUnreached : Family IdentityInput StagedOutput where
  inputs := (identityCases.filter fun (selected, _, _, parent) => parent != selected).map
    IdentityInput.ofCase
  model := fun i => { allowed := continuationIdentityValid i.selected i.signer i.actor i.parent }

structure TargetInput where
  name : String
  session : String
  observedSession : String
  agent : String
  observedAgent : String
  fresh : Bool
  commit : Bool
  deriving Lean.ToJson

structure TargetOutput where
  committed : Bool
  grants : Bool
  deriving Lean.ToJson

/-- `grants` is whether the User sees a request bound to the claimed session
and Agent. A forged case starts from an existing signed request whose targets
differ; that request is the observed target, not a publication. -/
@[conformance_family]
def claimedTarget : Family TargetInput TargetOutput where
  inputs :=
    [ ⟨"created", "session", "session", "agent", "agent", true, true⟩
    , ⟨"aborted", "session", "session", "agent", "agent", true, false⟩
    , ⟨"duplicate-after-revocation", "session", "session", "agent", "agent", false, true⟩
    , ⟨"wrong-session", "other", "session", "agent", "agent", true, true⟩
    , ⟨"wrong-agent", "session", "session", "other", "agent", true, true⟩
    , ⟨"missing-session", "", "session", "agent", "agent", true, true⟩
    , ⟨"missing-agent", "session", "session", "", "agent", true, true⟩ ]
  model := fun i =>
    let before : Identity.SessionChild.ClientRequestPublication :=
      if i.fresh then ⟨false, false, false⟩ else ⟨true, false, false⟩
    let after := commitClaimedRequest before "agent" "agent" "agent" "agent"
      i.session i.observedSession i.agent i.observedAgent i.fresh i.commit
    { committed := after.committed, grants := after.grants }

end Conformance.GoalClaimedPublicationContracts
