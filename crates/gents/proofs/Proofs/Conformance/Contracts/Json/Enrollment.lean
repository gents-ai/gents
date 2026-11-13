import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Contracts.Json.Core
import Proofs.P2P.Enrollment.AutoRenewal
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

structure TitleAdmissionTitleWire where
  text : String
  source : String
  deriving Lean.ToJson

structure TitleAdmissionPositionWire where
  slotRequestDocId : String
  replacesRequestDocId : String
  deriving Lean.ToJson

structure TitleAdmissionQueueWire where
  source : String
  policy : String
  key : Option String
  queuedAfterRequestId : Option String
  interruptedRequestId : Option String
  backgroundCompletionWakeVersion : Option Nat
  delivery : String
  position : Option TitleAdmissionPositionWire
  deriving Lean.ToJson

structure TitleAdmissionGoalWire where
  sequence : Int
  wrapup : Bool
  deriving Lean.ToJson

structure TitleAdmissionInputWire where
  selectedSkillIds : List String
  cwd : Option String
  initialTitle : Option TitleAdmissionTitleWire
  queue : Option TitleAdmissionQueueWire
  goalContinuation : Option TitleAdmissionGoalWire
  deriving Lean.ToJson

structure TitleAdmissionRequestWire where
  hop : Nat
  requestId : String
  purpose : String
  targetNode : String
  requesterDid : String
  agentDid : String
  sessionId : String
  content : String
  input : TitleAdmissionInputWire
  modelInputFieldsHex : List String
  createdAt : String
  triggerConfigDocumentId : String
  modelRetryFieldsHex : List String
  modelTriggerFieldsHex : List String
  modelParentFieldsHex : List String
  modelWorkspaceFieldsHex : List String
  modelSemanticFieldsHex : List String
  deriving Lean.ToJson

structure TitleAdmissionAdmissionWire where
  kind : String
  signerDid : String
  issuerDid : String
  sourceRequestId : String
  runtimeSourceKind : String
  signatureValid : Bool
  modelSignedFieldsHex : List String
  modelExpectedFieldsHex : List String
  deriving Lean.ToJson

structure TitleAdmissionParentWire where
  requestId : String
  documentId : String
  nodeDid : String
  sessionId : String
  agentDid : String
  logicalBindingCurrent : Bool
  physicalBindingCurrent : Bool
  deriving Lean.ToJson

structure TitleAdmissionEvidenceWire where
  sourceKind : String
  issuerDid : String
  sourceRequestId : String
  targetNode : String
  targetRuntimeAttestationValid : Bool
  sourceBindingCurrent : Bool
  sourceDocumentBindingCurrent : Bool
  titleParent : Option TitleAdmissionParentWire
  deriving Lean.ToJson

structure TitleAdmissionInput where
  name : String
  parentObservedState : String
  observationAvailable : Bool
  branchFieldsExact : Bool
  pendingDeadlineAbsent : Bool
  request : TitleAdmissionRequestWire
  admission : TitleAdmissionAdmissionWire
  runtimeEvidence : Option TitleAdmissionEvidenceWire
  sessionAgent : String
  deriving Lean.ToJson

private def titleHex (fields : Enrollment.CanonicalFields) : List String :=
  fields.map Enrollment.utf8HexString

private def titleSourceKindWire : Enrollment.RuntimeInternalSourceKind → String
  | .localControl => "local-control"
  | .automatedTrigger => "automated-trigger"

private def titleInputWire (input : Enrollment.RequestInput) : TitleAdmissionInputWire :=
  { selectedSkillIds := input.selectedSkillIds, cwd := input.cwd
    initialTitle := input.initialTitle.map fun title =>
      { text := title.text, source := title.source.toWireName }
    queue := input.queue.map fun queue =>
      { source := queue.source.toDefraDB, policy := queue.policy.toDefraDB, key := queue.key
        queuedAfterRequestId := queue.queuedAfterRequestId
        interruptedRequestId := queue.interruptedRequestId
        backgroundCompletionWakeVersion := queue.backgroundCompletionWakeVersion
        delivery := queue.delivery.toDefraDB
        position := queue.position.map fun p =>
          { slotRequestDocId := p.slotRequestDocId, replacesRequestDocId := p.replacesRequestDocId } }
    goalContinuation := input.goalContinuation.map fun goal =>
      { sequence := goal.sequence, wrapup := goal.wrapup } }

private def titleAdmissionInput (case : TitleRequestAdmissionCase) : TitleAdmissionInput :=
  let request := case.request
  let admission := case.admission
  { name := case.name, parentObservedState := case.parentObservedState.toDefraDB
    observationAvailable := case.observationAvailable, branchFieldsExact := case.branchFieldsExact
    pendingDeadlineAbsent := case.pendingDeadlineAbsent
    request :=
      { hop := request.hop, requestId := request.requestId, purpose := request.purpose.toWire
        targetNode := request.targetNode, requesterDid := request.requesterDid
        agentDid := request.agentDid, sessionId := request.sessionId, content := request.content
        input := titleInputWire request.input
        modelInputFieldsHex := titleHex (Enrollment.requestInputFields request.input)
        createdAt := request.createdAt, triggerConfigDocumentId := request.triggerConfigDocumentId
        modelRetryFieldsHex := titleHex request.retryFields
        modelTriggerFieldsHex := titleHex request.triggerFields
        modelParentFieldsHex := titleHex request.parentFields
        modelWorkspaceFieldsHex := titleHex (Enrollment.requestWorkspaceFields request.workspace)
        modelSemanticFieldsHex := titleHex (Enrollment.agentRequestSemanticFields request) }
    admission :=
      { kind := match admission.kind with
          | .enrollment => "enrollment"
          | .client => "client"
          | .localSelf => "local-self"
          | .runtimeInternal => "runtime-internal"
          | .peer => "peer"
        signerDid := admission.signerDid, issuerDid := admission.issuerDid
        sourceRequestId := admission.sourceRequestId
        runtimeSourceKind := titleSourceKindWire admission.runtimeSourceKind
        signatureValid := admission.signatureValid
        modelSignedFieldsHex := titleHex admission.signedFields
        modelExpectedFieldsHex := titleHex (Enrollment.agentRequestAdmissionFields request admission) }
    runtimeEvidence := case.runtimeEvidence.map fun evidence =>
      { sourceKind := titleSourceKindWire evidence.sourceKind, issuerDid := evidence.issuerDid
        sourceRequestId := evidence.sourceRequestId, targetNode := evidence.targetNode
        targetRuntimeAttestationValid := evidence.targetRuntimeAttestationValid
        sourceBindingCurrent := evidence.sourceBindingCurrent
        sourceDocumentBindingCurrent := evidence.sourceDocumentBindingCurrent
        titleParent := evidence.auxiliaryParent.map fun parent =>
          { requestId := parent.link.requestId, documentId := parent.link.documentId
            nodeDid := parent.nodeDid, sessionId := parent.sessionId, agentDid := parent.agentDid
            logicalBindingCurrent := parent.logicalBindingCurrent
            physicalBindingCurrent := parent.physicalBindingCurrent } }
    sessionAgent := case.sessionAgent }

private def titleDispositionWire : Enrollment.AgentRequestAdmissionDisposition → String
  | .admit => "admit"
  | .deny => "deny"
  | .retry => "retry"

private def titleAdmissionDisposition (i : TitleAdmissionInput) : String :=
  match titleRequestAdmissionCases.find? (·.name == i.name) with
  | some case => titleDispositionWire (Enrollment.auxiliaryPendingDisposition
      case.observationAvailable ({} : Enrollment.State) case.request case.admission
      case.runtimeEvidence case.sessionAgent case.branchFieldsExact case.pendingDeadlineAbsent
      CausalHop.defaultMaxRequestHop)
  | none => ""

private def titleAdmissionUnbound : List String :=
  [ "ordinary-signed-control-preserved", "pending-title-observation-unavailable"
  , "foreign-admission-branch-fields", "forged-physical-parent", "tool-linked-parent"
  , "signed-purpose-mutation", "missing-parent-evidence", "access-review-observation-unavailable" ]

@[conformance_family]
def titleAdmissionFreshVerifier : Family TitleAdmissionInput String where
  inputs := (titleRequestAdmissionCases.filter (!titleAdmissionUnbound.contains ·.name)).map
    titleAdmissionInput
  model := titleAdmissionDisposition

/-- model-production-gap: the native fresh-verifier fixture signs a physical
parent and the title request through native signing and admission records;
it has no representation for these eight modeled inputs (an ordinary request
on this path, unavailable observations, foreign branch fields, forged or
tool-linked parent tuples, a purpose changed after signing, missing parent
evidence). -/
@[conformance_family]
def titleAdmissionFreshVerifierUnreached : Family TitleAdmissionInput String where
  inputs := (titleRequestAdmissionCases.filter (titleAdmissionUnbound.contains ·.name)).map
    titleAdmissionInput
  model := titleAdmissionDisposition

structure TitlePurposeWireInput where
  name : String
  wire : Option String
  deriving Lean.ToJson

deriving instance Lean.ToJson for RequestPurpose

attribute [conformance_vocabulary] RequestPurpose.toWire

@[conformance_family]
def titlePurposeWire : Conformance.Family TitlePurposeWireInput (Option RequestPurpose) where
  inputs :=
    [ { name := "missing-purpose-is-invalid", wire := none }
    , { name := "unknown-purpose-is-invalid", wire := some "titleAudit" }
    , { name := "normal-purpose-explicit", wire := some "normal" }
    , { name := "title-purpose-explicit", wire := some "title-audit" }
    , { name := "access-review-purpose-explicit", wire := some "access-review" }
    , { name := "access-review-camel-case-is-invalid", wire := some "accessReview" } ]
  model := fun i => i.wire.bind RequestPurpose.fromWire?

end Conformance.Contracts

namespace Conformance.RequestAdmission

open Lean Enrollment Conformance.ContractCases

structure AdmissionInput where
  name : String
  observationAvailable : Bool
  kind : String
  signatureValid : Bool
  signedFieldsMatch : Bool
  branchFieldsExact : Bool
  pendingDeadlineAbsent : Bool
  signerMatchesRequester : Bool
  requesterMatchesTarget : Bool
  signerMatchesTarget : Bool
  signerMatchesIssuer : Bool
  requesterMatchesSessionScope : Bool
  currentApproval : Bool
  exactGeneration : Bool
  authorizationFresh : Bool
  runtimeEvidencePresent : Bool
  runtimeSourceKind : String
  targetRuntimeAttestationValid : Bool
  sourceBindingCurrent : Bool
  triggerConfigDocumentBindingCurrent : Bool
  sourceDocumentBindingCurrent : Bool
  targetPolicyAllows : Bool
  nativeCreateAuthorized : Bool
  hopWithinBound : Bool
  deriving ToJson

structure Admission where
  admitted : Bool
  disposition : String
  deriving ToJson, BEq

def AdmissionInput.ofCase (c : AgentRequestAdmissionCase) : AdmissionInput :=
  { name := c.name, observationAvailable := c.observationAvailable, kind := c.kind
    signatureValid := c.signatureValid, signedFieldsMatch := c.signedFieldsMatch
    branchFieldsExact := c.branchFieldsExact, pendingDeadlineAbsent := c.pendingDeadlineAbsent
    signerMatchesRequester := c.signerMatchesRequester
    requesterMatchesTarget := c.requesterMatchesTarget
    signerMatchesTarget := c.signerMatchesTarget, signerMatchesIssuer := c.signerMatchesIssuer
    requesterMatchesSessionScope := c.requesterMatchesSessionScope
    currentApproval := c.currentApproval, exactGeneration := c.exactGeneration
    authorizationFresh := c.authorizationFresh, runtimeEvidencePresent := c.runtimeEvidencePresent
    runtimeSourceKind := c.runtimeSourceKind
    targetRuntimeAttestationValid := c.targetRuntimeAttestationValid
    sourceBindingCurrent := c.sourceBindingCurrent
    triggerConfigDocumentBindingCurrent := c.triggerConfigDocumentBindingCurrent
    sourceDocumentBindingCurrent := c.sourceDocumentBindingCurrent
    targetPolicyAllows := c.targetPolicyAllows, nativeCreateAuthorized := c.nativeCreateAuthorized
    hopWithinBound := c.hopWithinBound }

private def kindOf (name : String) : AgentRequestAdmissionKind :=
  ([.enrollment, .client, .localSelf, .runtimeInternal, .peer].find?
    (requestAdmissionKindName · == name)).getD .client

private def sourceKindOf (name : String) : RuntimeInternalSourceKind :=
  ([.localControl, .automatedTrigger].find? (runtimeInternalSourceKindName · == name)).getD .localControl

def AdmissionInput.observation (i : AdmissionInput) : AgentRequestAdmissionObservation :=
  { kind := kindOf i.kind, signatureValid := i.signatureValid
    signedFieldsMatch := i.signedFieldsMatch, branchFieldsExact := i.branchFieldsExact
    pendingDeadlineAbsent := i.pendingDeadlineAbsent
    signerMatchesRequester := i.signerMatchesRequester
    requesterMatchesTarget := i.requesterMatchesTarget, signerMatchesTarget := i.signerMatchesTarget
    signerMatchesIssuer := i.signerMatchesIssuer
    requesterMatchesSessionScope := i.requesterMatchesSessionScope
    currentApproval := i.currentApproval, exactGeneration := i.exactGeneration
    authorizationFresh := i.authorizationFresh, runtimeEvidencePresent := i.runtimeEvidencePresent
    runtimeSourceKind := sourceKindOf i.runtimeSourceKind
    targetRuntimeAttestationValid := i.targetRuntimeAttestationValid
    sourceBindingCurrent := i.sourceBindingCurrent
    triggerConfigDocumentBindingCurrent := i.triggerConfigDocumentBindingCurrent
    sourceDocumentBindingCurrent := i.sourceDocumentBindingCurrent
    targetPolicyAllows := i.targetPolicyAllows, nativeCreateAuthorized := i.nativeCreateAuthorized
    hopWithinBound := i.hopWithinBound }

def admission (i : AdmissionInput) : Admission :=
  { admitted := projectAgentRequestAdmission i.observation
    disposition := requestAdmissionDispositionName
      (projectAgentRequestAdmissionDisposition i.observationAvailable i.observation) }

@[conformance_family]
def projected : Family AdmissionInput Admission where
  inputs := agentRequestAdmissionCases.map AdmissionInput.ofCase
  model := admission

/-- A served Agent can claim a historical local-self request whose requester
was never recorded; that is the one admission observation the background
process-control scenario persists and verifies natively. -/
@[conformance_family]
def absentRequester : Family AdmissionInput Admission where
  inputs := (agentRequestAdmissionCases.filter (·.name == "local-self-absent-requester")).map
    AdmissionInput.ofCase
  model := admission

theorem projected_model_matches_case_expectations :
    agentRequestAdmissionCases.all (fun c =>
      admission (AdmissionInput.ofCase c) == ⟨c.expectedAdmitted, c.expectedDisposition⟩) = true := by
  native_decide

end Conformance.RequestAdmission
