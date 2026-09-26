import Proofs.GoalAutomation.ClaimedPublication
import Proofs.Conformance.GoalOperatorResume

namespace Conformance.GoalClaimedPublicationContracts
open GoalAutomation.OperatorResume
open Conformance.GoalOperatorResumeContracts (parentBinding snapshotJson bindingJson)

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
/-- A pending accepted wait that request terminalization cancelled before it
started: settled, with no invocation reply. -/
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
  , ⟨"budget_wrapup_invalid_evidence_abandons_wrapup", budgetClaimed,
       {request with expectedStatus := .budgetLimited}, malformedWait, true,
       budgetWrapupAbandoned, .invalidEvidence⟩
  ]

theorem explicit_cases_replay : ∀ c ∈ cases,
    publishClaimed c.before c.request c.observation c.commit = (c.expected, c.outcome) := by decide

/-- The claim already exists when the wait is observed. A deferred publication
does not consume another claim, and target completion permits that same claim
to materialize its child on reconciliation. -/
theorem claimed_wait_then_completion_reuses_claim :
    let deferred := publishClaimed claimed request waiting true
    GoalAutomation.continuationStep .unclaimed (.claim true) = .claimed ∧
      deferred = (claimed, .deferred) ∧
      publishClaimed deferred.1 request completedTool true = (published, .created) := by decide


private def outcomeJson : Outcome → String
  | .denied => "denied" | .stale => "stale" | .illegal => "illegal"
  | .conflict => "conflict" | .rolledBack => "rolled_back"
  | .created => "created" | .recovered => "recovered"
  | .deferred => "deferred" | .invalidEvidence => "invalid_evidence"
  | .unavailable => "unavailable"

def backgroundJson (b : BackgroundTool) : String :=
  "{\"doc_id\":" ++ toString b.docId ++ ",\"origin\":" ++
    (match b.origin with
    | .spawned parent key => "{\"kind\":\"spawned\",\"parent_tool_doc\":" ++ toString parent ++
        ",\"stable_key\":" ++ Conformance.Contracts.jsonString key ++ "}"
    | .nonSpawned => "{\"kind\":\"non_spawned\"}") ++
  ",\"handle\":" ++ Conformance.Contracts.jsonString b.handle ++
  ",\"owner\":" ++ Conformance.Contracts.jsonString b.owner ++
  ",\"session\":" ++ Conformance.Contracts.jsonString b.session ++
  ",\"requester\":" ++ (match b.requester with | none => "null" | some x => Conformance.Contracts.jsonString x) ++
  ",\"state\":" ++ Conformance.Contracts.jsonString
    (match b.state with | .pending => "pending" | .running => "running" | .terminal => "terminal") ++ "}"

def waitJson (w : WaitControl) : String :=
  "{\"doc_id\":" ++ toString w.docId ++ ",\"parent_request_doc\":" ++ toString w.parentRequestDoc ++
  ",\"owner\":" ++ Conformance.Contracts.jsonString w.owner ++
  ",\"session\":" ++ Conformance.Contracts.jsonString w.session ++
  ",\"requester\":" ++ (match w.requester with | none => "null" | some x => Conformance.Contracts.jsonString x) ++
  ",\"accepted_tool\":" ++ Conformance.Contracts.jsonString w.acceptedTool ++
  ",\"accepted_handle\":" ++ Conformance.Contracts.jsonString w.acceptedHandle ++
  ",\"reply_handle\":" ++ Conformance.Contracts.jsonString w.replyHandle ++
  ",\"reply\":" ++ Conformance.Contracts.jsonString (match w.reply with | .timedOutRunning => "timed_out_running" | .other => "other" | .argumentError => "argument_error" | .settledDiagnostic => "settled_diagnostic" | .malformed => "malformed") ++
  ",\"terminal\":" ++ (if w.terminal then "true" else "false") ++
  ",\"replied\":" ++ (if w.replied then "true" else "false") ++ "}"

def observationJson (o : PublicationObservation) : String :=
  "{\"waits\":" ++ Conformance.Contracts.jsonArray (o.waits.map waitJson) ++
  ",\"backgrounds\":" ++
    (match o.backgrounds with
    | none => "null"
    | some rows => Conformance.Contracts.jsonArray (rows.map backgroundJson)) ++
  ",\"storage_failed\":" ++ (if o.storageFailed then "true" else "false") ++ "}"

def requestJson (r : ClaimedRequest) : String :=
  let base := Conformance.GoalOperatorResumeContracts.requestJson r.toRequest
  String.mk (base.toList.take (base.length - 1)) ++ ",\"expected_last_continued_from\":" ++
    (match r.expectedLastContinuedFrom with | none => "null" | some n => toString n) ++
    ",\"requester\":" ++ (match r.requester with | none => "null" | some x => Conformance.Contracts.jsonString x) ++ "}"

def caseJson (c : PublicationCase) : String :=
  "{\"name\":" ++ Conformance.Contracts.jsonString c.name ++
  ",\"before\":" ++ snapshotJson c.before ++ ",\"request\":" ++ requestJson c.request ++
  ",\"observation\":" ++ observationJson c.observation ++
  ",\"commit\":" ++ (if c.commit then "true" else "false") ++
  ",\"expected\":" ++ snapshotJson c.expected ++ ",\"outcome\":" ++
  Conformance.Contracts.jsonString (outcomeJson c.outcome) ++ "}"

def casesJson : String := Conformance.Contracts.jsonArray (cases.map caseJson)

end Conformance.GoalClaimedPublicationContracts
