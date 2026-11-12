import Proofs.Runtime.AuxiliaryAdmission
import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Session.Compaction.State
import Lean
import Proofs.Conformance.Family

namespace Conformance.TitleAdmission

open Lean CanonicalOutput.Execution

private def ids : _root_.AuxiliaryAdmission.Identities :=
  { encode := Compaction.PromptView.encodeProviderKey
  , injective := Compaction.PromptView.encodeProviderKey_injective }

private def baseRequest := Conformance.ContractCases.titleRequest
private def baseAdmission := Conformance.ContractCases.signTitleAdmission baseRequest
private def evidence : Option Enrollment.RuntimeInternalEvidence :=
  some Conformance.ContractCases.titleEvidence

private def baseRow : _root_.AuxiliaryAdmission.RequestRowEvidence :=
  { physicalRequest := "T"
  , logicalRequest := baseRequest.requestId
  , signedFields := Enrollment.agentRequestAdmissionFields baseRequest baseAdmission
  , physicalBindingCurrent := true
  , branchFieldsExact := true
  , pendingDeadlineAbsent := true
  , maxRequestHop := CausalHop.defaultMaxRequestHop }

private structure Case where
  name : String
  available : Bool := true
  request : Enrollment.AgentRequestSemantics := baseRequest
  admission : Enrollment.AgentRequestAdmission := baseAdmission
  admissionCase : String := "valid-title-parent-completed"
  row : _root_.AuxiliaryAdmission.RequestRowEvidence := baseRow
  ownPhysical : String := "T"
  activeNormal : Bool := false

private def admittedCase : Case :=
  { name := "admitted-pending-title-claims-own-lease" }
private def activeQueueCase : Case :=
  { name := "admitted-title-while-normal-queue-active", activeNormal := true }
private def wrongWorldPhysicalCase : Case :=
  { name := "own-physical-world-mismatch", ownPhysical := "X" }
private def unavailableCase : Case :=
  { name := "observation-unavailable", available := false }

private def reviewRequest : Enrollment.AgentRequestSemantics :=
  { baseRequest with purpose := .accessReview, content := "Review workspace access" }

private def reviewAdmission := Conformance.ContractCases.signTitleAdmission reviewRequest

private def reviewRow : _root_.AuxiliaryAdmission.RequestRowEvidence :=
  { baseRow with signedFields := Enrollment.agentRequestAdmissionFields reviewRequest reviewAdmission }

private def reviewCase : Case :=
  { name := "access-review-claims-independent-lease-with-active-parent"
  , request := reviewRequest, admission := reviewAdmission, row := reviewRow
  , admissionCase := "valid-access-review-parent-processing", activeNormal := true }

private def cases : List Case :=
  [ reviewCase
  , { reviewCase with name := "access-review-signed-row-cannot-replay-title", row := baseRow }
  , { reviewCase with name := "access-review-observation-unavailable", available := false }
  , admittedCase
  , activeQueueCase
  , { name := "row-logical-request-mismatch"
    , row := { baseRow with logicalRequest := "x" } }
  , { name := "row-signed-fields-mismatch"
    , row := { baseRow with signedFields := [] } }
  , { name := "row-physical-binding-stale"
    , row := { baseRow with physicalBindingCurrent := false } }
  , wrongWorldPhysicalCase
  , unavailableCase
  , { name := "foreign-branch-fields"
    , row := { baseRow with branchFieldsExact := false } }
  , { name := "present-preclaim-deadline"
    , row := { baseRow with pendingDeadlineAbsent := false } } ]

private def world (c : Case) : World :=
  let request := c.request
  let session := ids.encode request.sessionId
  let node := ids.encode request.targetNode
  let active := if c.activeNormal then some (ids.encode "n") else none
  let queue : SessionQueue.SessionQueueState :=
    { scope := { node := node, session := session, requester := none }, active := active, pending := [], terminal := ∅ }
  { requestId := ids.encode c.ownPhysical
  , sessionId := session
  , purpose := request.purpose
  , nodeDid := node
  , lease := RequestExecutionLease.initial Nat
  , segments := []
  , messages := []
  , transcript := { sessionId := session, nextSeq := 0, messages := [], toolCalls := [], inFlight := ∅ }
  , terminalSelection := none
  , gateOwner := some 9
  , gateSchedule := ⟨.storage, true, false⟩
  , queue := queue }

private def activation (c : Case) : Option Handover.AuxiliaryActivation :=
  _root_.AuxiliaryAdmission.activation? ids c.available ({} : Enrollment.State)
    c.request c.admission evidence c.request.agentDid c.row 7 20 30

private def claimed (c : Case) : Option World :=
  _root_.AuxiliaryAdmission.activate? (world c) 9 1 ids c.available
    ({} : Enrollment.State) c.request c.admission evidence c.request.agentDid
    c.row 7 20 30 99
    { transportRetries := 0, resampleRetries := 0, allowRepair := false } none

private theorem admitted_claims_live_lease :
    (claimed admittedCase).isSome = true ∧
      (match (claimed admittedCase).map (·.lease.lease) with
       | some (.active _ _ _) => true
       | _ => false) = true := by
  native_decide

private theorem active_normal_queue_is_not_consumed :
    (claimed activeQueueCase).isSome = true ∧
      (claimed activeQueueCase).map (·.queue) = some (world activeQueueCase).queue := by
  native_decide

private theorem review_preserves_the_active_queue_and_binds_its_purpose :
    (claimed reviewCase).isSome = true ∧
      (claimed reviewCase).map (·.queue) = some (world reviewCase).queue ∧
      ((activation reviewCase).map (·.binding.purpose)) = some .accessReview := by
  native_decide

private theorem wrong_world_physical_rejects_valid_activation :
    (activation wrongWorldPhysicalCase).isSome = true ∧
      (claimed wrongWorldPhysicalCase).isNone = true := by
  native_decide

private theorem unavailable_observation_retries_without_claim :
    Enrollment.auxiliaryPendingDisposition false ({} : Enrollment.State) baseRequest baseAdmission
      evidence baseRequest.agentDid unavailableCase.row.branchFieldsExact
      unavailableCase.row.pendingDeadlineAbsent unavailableCase.row.maxRequestHop = .retry ∧
      (claimed unavailableCase).isNone = true := by
  native_decide

private def emittedModelLabels (c : Case) : List Nat :=
  let start := world c
  let binding := (activation c).map fun activation =>
    let value := activation.binding
    [value.physicalRequest, value.logicalRequest, value.parentPhysical,
     value.parentLogical, value.node, value.session]
  [start.requestId, start.sessionId, start.nodeDid,
   start.queue.scope.node, start.queue.scope.session,
   start.queue.active.getD 0] ++ binding.getD []

private theorem emitted_symbol_ids_remain_bounded :
    cases.all (fun c => (emittedModelLabels c).all (· < 65536)) = true := by
  native_decide

inductive SignedFor where
  | title
  | accessReview
  | nothing
  deriving ToJson, DecidableEq

/-- The scenario knobs of one join case: which signed request the runtime
observes, which request the row's signed fields were produced for, and the
row and world facts. `admissionCase` names the admission case the request was
signed as. -/
structure JoinInput where
  name : String
  admissionCase : String
  observationAvailable : Bool
  accessReview : Bool
  rowSignedFor : SignedFor
  rowPhysicalRequest : String
  rowLogicalRequest : String
  physicalBindingCurrent : Bool
  branchFieldsExact : Bool
  pendingDeadlineAbsent : Bool
  ownPhysical : String
  activeNormal : Bool
  deriving ToJson

structure BindingObservation where
  purpose : String
  physicalRequest : Nat
  logicalRequest : Nat
  parentPhysical : Nat
  parentLogical : Nat
  node : Nat
  session : Nat
  authenticated : Bool
  deriving ToJson, DecidableEq

structure StartObservation where
  ownPhysical : Nat
  purpose : String
  session : Nat
  nodeDid : Nat
  queueScopeNode : Nat
  queueScopeSession : Nat
  queueActive : Option Nat
  leasePending : Bool
  leaseVacant : Bool
  leaseNow : Nat
  claimedAbsent : Bool
  terminalSelectionAbsent : Bool
  gateIndependent : Bool
  gateSiblingWaiting : Bool
  deriving ToJson, DecidableEq

structure JoinObservation where
  start : StartObservation
  activation : Option BindingObservation
  claimed : Bool
  leaseActive : Bool
  claimedBinding : Option BindingObservation
  queueActive : Option Nat
  queueUnchanged : Bool
  ownPhysical : Nat
  deriving ToJson, DecidableEq

private def BindingObservation.ofModel (b : Handover.AuxiliaryBinding) : BindingObservation :=
  { purpose := b.purpose.toWire, physicalRequest := b.physicalRequest
    logicalRequest := b.logicalRequest, parentPhysical := b.parentPhysical
    parentLogical := b.parentLogical, node := b.node, session := b.session
    authenticated := b.authenticated }

private def observe (c : Case) : JoinObservation :=
  let start := world c
  let result := claimed c
  { start :=
      { ownPhysical := start.requestId, purpose := start.purpose.toWire
        session := start.sessionId, nodeDid := start.nodeDid
        queueScopeNode := start.queue.scope.node, queueScopeSession := start.queue.scope.session
        queueActive := start.queue.active
        leasePending := start.lease.request == .pending
        leaseVacant := start.lease.lease == .vacant
        leaseNow := start.lease.now
        claimedAbsent := start.claimed.isNone
        terminalSelectionAbsent := start.terminalSelection.isNone
        gateIndependent := start.gateSchedule.independent
        gateSiblingWaiting := start.gateSchedule.siblingWaiting }
    activation := (activation c).map fun value => BindingObservation.ofModel value.binding
    claimed := result.isSome
    leaseActive := match result with
      | some after => match after.lease.lease with
        | .active _ _ _ => true
        | _ => false
      | none => false
    claimedBinding := (result.bind (·.claimed)).bind fun binding =>
      match binding.evidence with
      | .auxiliary title => some (BindingObservation.ofModel title)
      | _ => none
    queueActive := (result.getD start).queue.active
    queueUnchanged := (result.getD start).queue == start.queue
    ownPhysical := (result.getD start).requestId }

private def signedFor (row : _root_.AuxiliaryAdmission.RequestRowEvidence) : SignedFor :=
  if row.signedFields == baseRow.signedFields then .title
  else if row.signedFields == reviewRow.signedFields then .accessReview
  else .nothing

private def JoinInput.ofCase (c : Case) : JoinInput :=
  { name := c.name, admissionCase := c.admissionCase, observationAvailable := c.available
    accessReview := c.request.purpose == .accessReview, rowSignedFor := signedFor c.row
    rowPhysicalRequest := c.row.physicalRequest, rowLogicalRequest := c.row.logicalRequest
    physicalBindingCurrent := c.row.physicalBindingCurrent
    branchFieldsExact := c.row.branchFieldsExact
    pendingDeadlineAbsent := c.row.pendingDeadlineAbsent
    ownPhysical := c.ownPhysical, activeNormal := c.activeNormal }

private def JoinInput.toCase (i : JoinInput) : Case :=
  { name := i.name, available := i.observationAvailable
    request := if i.accessReview then reviewRequest else baseRequest
    admission := if i.accessReview then reviewAdmission else baseAdmission
    admissionCase := i.admissionCase
    row :=
      { baseRow with
        physicalRequest := i.rowPhysicalRequest, logicalRequest := i.rowLogicalRequest
        signedFields := match i.rowSignedFor with
          | .title => baseRow.signedFields
          | .accessReview => reviewRow.signedFields
          | .nothing => []
        physicalBindingCurrent := i.physicalBindingCurrent
        branchFieldsExact := i.branchFieldsExact
        pendingDeadlineAbsent := i.pendingDeadlineAbsent }
    ownPhysical := i.ownPhysical, activeNormal := i.activeNormal }

example : cases.all (fun c => observe (JoinInput.toCase (JoinInput.ofCase c)) == observe c) = true := by
  native_decide

/-- model-production-gap: the join is executed only through the Lean title
activation and claim owner; no production scenario materializes the observed
row and own-lease world for the title or access-review claim, so neither the
native watcher admission nor the claim owner replays these cases. -/
@[conformance_family]
def joinUnreached : Conformance.Family JoinInput JoinObservation where
  inputs := cases.map JoinInput.ofCase
  model := fun i => observe (JoinInput.toCase i)

end Conformance.TitleAdmission
