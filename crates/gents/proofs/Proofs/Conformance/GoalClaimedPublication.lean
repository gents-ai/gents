import Proofs.GoalAutomation.ClaimedPublication
import Proofs.Conformance.GoalOperatorResume

namespace Conformance.GoalClaimedPublicationContracts
open GoalAutomation.OperatorResume
open Conformance.GoalOperatorResumeContracts (parentBinding snapshotJson bindingJson)

def claimed : Snapshot :=
  ⟨⟨.active, 2, false, false⟩, 1, some 10, 10, [], 37, some 1000⟩
def request : ClaimedRequest :=
  ⟨⟨.active, 1, true, true, true, true, parentBinding⟩, some 10⟩
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

structure PublicationCase where
  name : String
  before : Snapshot
  request : ClaimedRequest
  commit : Bool
  expected : Snapshot
  outcome : Outcome
  deriving DecidableEq, Repr

def cases : List PublicationCase :=
  [ ⟨"claimed_active_publishes_without_reclaim", claimed, request, true, published, .created⟩
  , ⟨"claimed_budget_wrapup_publishes", budgetClaimed,
       {request with expectedStatus := .budgetLimited}, true,
       {published with goal := budgetClaimed.goal}, .created⟩
  , ⟨"pause_preempts_unpublished_child", paused, request, true, paused, .stale⟩
  , ⟨"new_resume_epoch_preempts_old_claim", laterEpoch, request, true, laterEpoch, .stale⟩
  , ⟨"changed_parent_watermark_preempts", changedWatermark, request, true, changedWatermark, .stale⟩
  , ⟨"discard_publishes_nothing", claimed, request, false, claimed, .rolledBack⟩
  , ⟨"exact_receipt_recovers_after_pause_and_progress", recoveredLater, request, true,
       recoveredLater, .recovered⟩
  , ⟨"foreign_receipt_cannot_recover", {published with children :=
       [{parentBinding with semanticFingerprint := "foreign"}]}, request, true,
       {published with children := [{parentBinding with semanticFingerprint := "foreign"}]}, .conflict⟩
  ]

theorem explicit_cases_replay : ∀ c ∈ cases,
    publishClaimed c.before c.request c.commit = (c.expected, c.outcome) := by decide


private def outcomeJson : Outcome → String
  | .denied => "denied" | .stale => "stale" | .illegal => "illegal"
  | .conflict => "conflict" | .rolledBack => "rolled_back"
  | .created => "created" | .recovered => "recovered"

def requestJson (r : ClaimedRequest) : String :=
  let base := Conformance.GoalOperatorResumeContracts.requestJson r.toRequest
  String.mk (base.toList.take (base.length - 1)) ++ ",\"expected_last_continued_from\":" ++
    (match r.expectedLastContinuedFrom with | none => "null" | some n => toString n) ++ "}"

def caseJson (c : PublicationCase) : String :=
  "{\"name\":" ++ Conformance.Contracts.jsonString c.name ++
  ",\"before\":" ++ snapshotJson c.before ++ ",\"request\":" ++ requestJson c.request ++
  ",\"commit\":" ++ (if c.commit then "true" else "false") ++
  ",\"expected\":" ++ snapshotJson c.expected ++ ",\"outcome\":" ++
  Conformance.Contracts.jsonString (outcomeJson c.outcome) ++ "}"

def casesJson : String := Conformance.Contracts.jsonArray (cases.map caseJson)

end Conformance.GoalClaimedPublicationContracts
