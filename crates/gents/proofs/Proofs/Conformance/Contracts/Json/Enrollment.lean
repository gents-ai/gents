import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Contracts.Json.Core
import Proofs.P2P.Enrollment.AutoRenewal
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

private def boolJson (value : Bool) : String := if value then "true" else "false"

def agentRequestAdmissionCaseJson (c : AgentRequestAdmissionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"observation_available\":" ++ boolJson c.observationAvailable ++ ","
    ++ "\"kind\":" ++ jsonString c.kind ++ ","
    ++ "\"signature_valid\":" ++ boolJson c.signatureValid ++ ","
    ++ "\"signed_fields_match\":" ++ boolJson c.signedFieldsMatch ++ ","
    ++ "\"branch_fields_exact\":" ++ boolJson c.branchFieldsExact ++ ","
    ++ "\"pending_deadline_absent\":" ++ boolJson c.pendingDeadlineAbsent ++ ","
    ++ "\"signer_matches_requester\":" ++ boolJson c.signerMatchesRequester ++ ","
    ++ "\"requester_matches_target\":" ++ boolJson c.requesterMatchesTarget ++ ","
    ++ "\"signer_matches_target\":" ++ boolJson c.signerMatchesTarget ++ ","
    ++ "\"signer_matches_issuer\":" ++ boolJson c.signerMatchesIssuer ++ ","
    ++ "\"requester_matches_session_scope\":" ++ boolJson c.requesterMatchesSessionScope ++ ","
    ++ "\"current_approval\":" ++ boolJson c.currentApproval ++ ","
    ++ "\"exact_generation\":" ++ boolJson c.exactGeneration ++ ","
    ++ "\"authorization_fresh\":" ++ boolJson c.authorizationFresh ++ ","
    ++ "\"runtime_evidence_present\":" ++ boolJson c.runtimeEvidencePresent ++ ","
    ++ "\"runtime_source_kind\":" ++ jsonString c.runtimeSourceKind ++ ","
    ++ "\"target_runtime_attestation_valid\":" ++
      boolJson c.targetRuntimeAttestationValid ++ ","
    ++ "\"source_binding_current\":" ++ boolJson c.sourceBindingCurrent ++ ","
    ++ "\"trigger_config_document_binding_current\":" ++
      boolJson c.triggerConfigDocumentBindingCurrent ++ ","
    ++ "\"source_document_binding_current\":" ++ boolJson c.sourceDocumentBindingCurrent ++ ","
    ++ "\"target_policy_allows\":" ++ boolJson c.targetPolicyAllows ++ ","
    ++ "\"native_create_authorized\":" ++ boolJson c.nativeCreateAuthorized ++ ","
    ++ "\"hop_within_bound\":" ++ boolJson c.hopWithinBound ++ ","
    ++ "\"expected_admitted\":" ++ boolJson c.expectedAdmitted ++ ","
    ++ "\"expected_disposition\":" ++ jsonString c.expectedDisposition
    ++ "}"

def agentRequestAdmissionCasesJson : String :=
  jsonArray (agentRequestAdmissionCases.map agentRequestAdmissionCaseJson)

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
