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

/-- Unbuildable in production: admission reloads the request by its own
document and claims that same document, so the observed row is always the
claimed request's current physical row and the claim always targets it. The
model still refuses both. -/
private theorem foreign_physical_rows_never_claim :
    (claimed { admittedCase with
      name := "row-physical-binding-stale"
      row := { baseRow with physicalBindingCurrent := false } }).isNone = true ∧
      (claimed wrongWorldPhysicalCase).isNone = true := by
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
  rowLogicalRequest : String
  branchFieldsExact : Bool
  pendingDeadlineAbsent : Bool
  activeNormal : Bool
  deriving ToJson

/-- What a claim of the auxiliary request shows: the purpose its activation
binds, whether it claimed its own lease, whether that lease is active, and
whether the session's normal queue is untouched. -/
structure JoinObservation where
  activatedPurpose : Option String
  claimed : Bool
  leaseActive : Bool
  queueUnchanged : Bool
  deriving ToJson, DecidableEq

private def observe (c : Case) : JoinObservation :=
  let start := world c
  let result := claimed c
  { activatedPurpose := (activation c).map (·.binding.purpose.toWire)
    claimed := result.isSome
    leaseActive := match result with
      | some after => match after.lease.lease with
        | .active _ _ _ => true
        | _ => false
      | none => false
    queueUnchanged := (result.getD start).queue == start.queue }

private def signedFor (row : _root_.AuxiliaryAdmission.RequestRowEvidence) : SignedFor :=
  if row.signedFields == baseRow.signedFields then .title
  else if row.signedFields == reviewRow.signedFields then .accessReview
  else .nothing

private def JoinInput.ofCase (c : Case) : JoinInput :=
  { name := c.name, admissionCase := c.admissionCase, observationAvailable := c.available
    accessReview := c.request.purpose == .accessReview, rowSignedFor := signedFor c.row
    rowLogicalRequest := c.row.logicalRequest
    branchFieldsExact := c.row.branchFieldsExact
    pendingDeadlineAbsent := c.row.pendingDeadlineAbsent
    activeNormal := c.activeNormal }

private def JoinInput.toCase (i : JoinInput) : Case :=
  { name := i.name, available := i.observationAvailable
    request := if i.accessReview then reviewRequest else baseRequest
    admission := if i.accessReview then reviewAdmission else baseAdmission
    admissionCase := i.admissionCase
    row :=
      { baseRow with
        logicalRequest := i.rowLogicalRequest
        signedFields := match i.rowSignedFor with
          | .title => baseRow.signedFields
          | .accessReview => reviewRow.signedFields
          | .nothing => []
        branchFieldsExact := i.branchFieldsExact
        pendingDeadlineAbsent := i.pendingDeadlineAbsent }
    activeNormal := i.activeNormal }

example : cases.all (fun c => observe (JoinInput.toCase (JoinInput.ofCase c)) == observe c) = true := by
  native_decide

/-- Admission of the auxiliary request's own row followed by the claim of its
own lease, replayed through the fresh verifier and the request claim owner on
a served node. -/
@[conformance_family]
def join : Conformance.Family JoinInput JoinObservation where
  inputs := cases.map JoinInput.ofCase
  model := fun i => observe (JoinInput.toCase i)

end Conformance.TitleAdmission
