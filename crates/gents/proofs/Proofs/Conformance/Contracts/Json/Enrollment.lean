import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Contracts.Json.Core
import Proofs.P2P.Enrollment.AutoRenewal
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

private def boolJson (value : Bool) : String := if value then "true" else "false"

def renewalIntentCasesJson : String :=
  jsonArray (Enrollment.AutoRenewal.intentBoundaryCases.map fun (name, g, i, now, window, valid) =>
    "{\"name\":" ++ jsonString name ++
      ",\"current\":{\"network\":" ++ jsonString g.network ++
      ",\"member\":" ++ jsonString g.member ++ ",\"peer\":" ++ jsonString g.peer ++
      ",\"expires_at\":" ++ toString g.expiresAt ++ ",\"sequence\":" ++ toString g.sequence ++
      ",\"next_sequence\":" ++ toString g.nextSequence ++ ",\"conflict\":" ++ boolJson g.authorityConflict ++
      ",\"revoked\":" ++ boolJson g.revoked ++ ",\"enabled\":" ++ boolJson g.enabled ++
      ",\"verified\":" ++ boolJson g.verified ++ "}" ++
      ",\"intent\":{\"network\":" ++ jsonString i.network ++
      ",\"member\":" ++ jsonString i.member ++ ",\"peer\":" ++ jsonString i.peer ++
      ",\"predecessor_sequence\":" ++ toString i.predecessorSequence ++
      ",\"issued_at\":" ++ toString i.issuedAt ++ ",\"expires_at\":" ++ toString i.expiresAt ++
      ",\"member_signed\":" ++ boolJson i.memberSigned ++
      ",\"transport_authenticated\":" ++ boolJson i.transportAuthenticated ++ "}" ++
      ",\"now\":" ++ toString now ++ ",\"window\":" ++ toString window ++
      ",\"max_validity\":" ++ toString valid ++ ",\"allowed\":" ++
      boolJson (Enrollment.AutoRenewal.mayOffer g i now window valid) ++
      ",\"projection_allowed\":" ++ boolJson (Enrollment.AutoRenewal.offerProjection g i now window valid) ++ "}")

def autoRenewalCasesJson : String :=
  jsonArray (Enrollment.AutoRenewal.boundaryInputCases.map fun c =>
    let g := c.current
    let p := c.proposal
    let limit := match g.renewalLeaseLimit with | none => "null" | some n => toString n
    "{\"name\":" ++ jsonString c.name ++
      ",\"current\":{\"network\":" ++ jsonString g.network ++
      ",\"member\":" ++ jsonString g.member ++ ",\"peer\":" ++ jsonString g.peer ++
      ",\"owner\":" ++ jsonString g.owner ++ ",\"profile\":" ++ jsonString g.profile ++
      ",\"expires_at\":" ++ toString g.expiresAt ++ ",\"lease_seconds\":" ++ toString g.leaseSeconds ++
      ",\"renewal_limit\":" ++ limit ++ ",\"sequence\":" ++ toString g.sequence ++ ",\"revoked\":" ++ boolJson g.revoked ++
      ",\"next_sequence\":" ++ toString g.nextSequence ++ ",\"authority_conflict\":" ++ boolJson g.authorityConflict ++
      ",\"enabled\":" ++ boolJson g.enabled ++ ",\"verified\":" ++ boolJson g.verified ++ "}" ++
      ",\"proposal\":{\"network\":" ++ jsonString p.network ++
      ",\"member\":" ++ jsonString p.member ++ ",\"peer\":" ++ jsonString p.peer ++
      ",\"owner\":" ++ jsonString p.owner ++ ",\"profile\":" ++ jsonString p.profile ++
      ",\"member_signed\":" ++ boolJson p.memberSigned ++ ",\"offer_fresh\":" ++ boolJson p.offerFresh ++
      ",\"predecessor_sequence\":" ++ toString p.predecessorSequence ++ "}" ++
      ",\"now\":" ++ toString c.now ++ ",\"window\":" ++ toString c.window ++
      ",\"protocol_max\":" ++ toString c.protocolMax ++ ",\"allowed\":" ++
      boolJson (Enrollment.AutoRenewal.allowed g p c.now c.window c.protocolMax) ++
      ",\"projection_allowed\":" ++ boolJson (Enrollment.AutoRenewal.approvalProjection g p c.now c.window c.protocolMax) ++ "}")

def enrollmentTraceStepJson (step : EnrollmentTraceStep) : String :=
  "{"
    ++ "\"action\":" ++ jsonString step.action ++ ","
    ++ "\"peer_admission_did\":" ++ jsonString step.peerAdmissionDid ++ ","
    ++ "\"offer_id\":" ++ jsonString step.offerId ++ ","
    ++ "\"offer_challenge\":" ++ jsonString step.offerChallenge ++ ","
    ++ "\"offer_network_id\":" ++ jsonString step.offerNetworkId ++ ","
    ++ "\"offer_admin_did\":" ++ jsonString step.offerAdminDid ++ ","
    ++ "\"offer_server_peer\":" ++ jsonString step.offerServerPeer ++ ","
    ++ "\"offer_owner_node\":" ++ jsonString step.offerOwnerNode ++ ","
    ++ "\"offer_profile\":" ++ jsonString step.offerProfile ++ ","
    ++ "\"challenge\":" ++ jsonString step.challenge ++ ","
    ++ "\"request_id\":" ++ jsonString step.requestId ++ ","
    ++ "\"request_digest\":" ++ jsonString step.requestDigest ++ ","
    ++ "\"request_offer_id\":" ++ jsonString step.requestOfferId ++ ","
    ++ "\"network_id\":" ++ jsonString step.networkId ++ ","
    ++ "\"admin_did\":" ++ jsonString step.adminDid ++ ","
    ++ "\"server_peer\":" ++ jsonString step.serverPeer ++ ","
    ++ "\"server_ticket_peer\":" ++ jsonString step.serverTicketPeer ++ ","
    ++ "\"resolved_server_did\":" ++ jsonString step.resolvedServerDid ++ ","
    ++ "\"profile\":" ++ jsonString step.profile ++ ","
    ++ "\"schema_compatible\":" ++ boolJson step.schemaCompatible ++ ","
    ++ "\"offer_admin_signed\":" ++ boolJson step.offerAdminSigned ++ ","
    ++ "\"offer_fresh\":" ++ boolJson step.offerFresh ++ ","
    ++ "\"candidate_did\":" ++ jsonString step.candidateDid ++ ","
    ++ "\"candidate_peer\":" ++ jsonString step.candidatePeer ++ ","
    ++ "\"observed_candidate_peer\":" ++ jsonString step.observedCandidatePeer ++ ","
    ++ "\"resolved_candidate_did\":" ++ jsonString step.resolvedCandidateDid ++ ","
    ++ "\"candidate_ticket_peer\":" ++ jsonString step.candidateTicketPeer ++ ","
    ++ "\"owner_node\":" ++ jsonString step.ownerNode ++ ","
    ++ "\"client_nonce\":" ++ jsonString step.clientNonce ++ ","
    ++ "\"issued_at\":" ++ jsonString step.issuedAt ++ ","
    ++ "\"expires_at\":" ++ jsonString step.expiresAt ++ ","
    ++ "\"candidate_signed\":" ++ boolJson step.candidateSigned ++ ","
    ++ "\"request_fresh\":" ++ boolJson step.requestFresh ++ ","
    ++ "\"decision_authorization_sequence\":" ++
      toString step.decisionAuthorizationSequence ++ ","
    ++ "\"decision_authorization_expires_at\":" ++
      jsonString step.decisionAuthorizationExpiresAt ++ ","
    ++ "\"decision_signer_did\":" ++ jsonString step.decisionSignerDid ++ ","
    ++ "\"decision_kind\":" ++ jsonString step.decisionKind ++ ","
    ++ "\"decision_request_id\":" ++ jsonString step.decisionRequestId ++ ","
    ++ "\"decision_request_digest\":" ++ jsonString step.decisionRequestDigest ++ ","
    ++ "\"decision_network_id\":" ++ jsonString step.decisionNetworkId ++ ","
    ++ "\"decision_admin_did\":" ++ jsonString step.decisionAdminDid ++ ","
    ++ "\"decision_candidate_did\":" ++ jsonString step.decisionCandidateDid ++ ","
    ++ "\"decision_candidate_peer\":" ++ jsonString step.decisionCandidatePeer ++ ","
    ++ "\"decision_owner_node\":" ++ jsonString step.decisionOwnerNode ++ ","
    ++ "\"decision_admin_signed\":" ++ boolJson step.decisionAdminSigned ++ ","
    ++ "\"decision_fresh\":" ++ boolJson step.decisionFresh ++ ","
    ++ "\"revision_kind\":" ++ jsonString step.revisionKind ++ ","
    ++ "\"revision_sequence\":" ++ toString step.revisionSequence ++ ","
    ++ "\"revision_authorization_expires_at\":" ++
      jsonString step.revisionAuthorizationExpiresAt ++ ","
    ++ "\"revision_signer_did\":" ++ jsonString step.revisionSignerDid ++ ","
    ++ "\"revision_request_id\":" ++ jsonString step.revisionRequestId ++ ","
    ++ "\"revision_request_digest\":" ++ jsonString step.revisionRequestDigest ++ ","
    ++ "\"revision_network_id\":" ++ jsonString step.revisionNetworkId ++ ","
    ++ "\"revision_admin_did\":" ++ jsonString step.revisionAdminDid ++ ","
    ++ "\"revision_member_did\":" ++ jsonString step.revisionMemberDid ++ ","
    ++ "\"revision_member_peer\":" ++ jsonString step.revisionMemberPeer ++ ","
    ++ "\"revision_owner_node\":" ++ jsonString step.revisionOwnerNode ++ ","
    ++ "\"revision_admin_signed\":" ++ boolJson step.revisionAdminSigned ++ ","
    ++ "\"receipt_request_id\":" ++ jsonString step.receiptRequestId ++ ","
    ++ "\"receipt_request_digest\":" ++ jsonString step.receiptRequestDigest ++ ","
    ++ "\"receipt_network_id\":" ++ jsonString step.receiptNetworkId ++ ","
    ++ "\"receipt_admin_did\":" ++ jsonString step.receiptAdminDid ++ ","
    ++ "\"receipt_member_did\":" ++ jsonString step.receiptMemberDid ++ ","
    ++ "\"receipt_member_peer\":" ++ jsonString step.receiptMemberPeer ++ ","
    ++ "\"receipt_server_peer\":" ++ jsonString step.receiptServerPeer ++ ","
    ++ "\"receipt_owner_node\":" ++ jsonString step.receiptOwnerNode ++ ","
    ++ "\"receipt_authorization_sequence\":" ++
      toString step.receiptAuthorizationSequence ++ ","
    ++ "\"receipt_authorization_expires_at\":" ++
      jsonString step.receiptAuthorizationExpiresAt ++ ","
    ++ "\"receipt_direction\":" ++ jsonString step.receiptDirection ++ ","
    ++ "\"receipt_signer_did\":" ++ jsonString step.receiptSignerDid ++ ","
    ++ "\"receipt_admin_signed\":" ++ boolJson step.receiptAdminSigned ++ ","
    ++ "\"receipt_applied\":" ++ boolJson step.receiptApplied ++ ","
    ++ "\"observed_offer_count\":" ++ toString step.observedOfferCount ++ ","
    ++ "\"admin_pin_count\":" ++ toString step.adminPinCount ++ ","
    ++ "\"challenge_binding_count\":" ++ toString step.challengeBindingCount ++ ","
    ++ "\"request_binding_count\":" ++ toString step.requestBindingCount ++ ","
    ++ "\"request_count\":" ++ toString step.requestCount ++ ","
    ++ "\"decision_count\":" ++ toString step.decisionCount ++ ","
    ++ "\"authorization_count\":" ++ toString step.authorizationCount ++ ","
    ++ "\"membership_count\":" ++ toString step.membershipCount ++ ","
    ++ "\"receipt_count\":" ++ toString step.receiptCount ++ ","
    ++ "\"route_count\":" ++ toString step.routeCount ++ ","
    ++ "\"request_accepted\":" ++ boolJson step.requestAccepted ++ ","
    ++ "\"decision_recorded\":" ++ boolJson step.decisionRecorded ++ ","
    ++ "\"authorization_recorded\":" ++ boolJson step.authorizationRecorded ++ ","
    ++ "\"revision_recorded\":" ++ boolJson step.revisionRecorded ++ ","
    ++ "\"receipt_recorded\":" ++ boolJson step.receiptRecorded ++ ","
    ++ "\"membership_present\":" ++ boolJson step.membershipPresent ++ ","
    ++ "\"client_route_present\":" ++ boolJson step.clientRoutePresent ++ ","
    ++ "\"server_route_present\":" ++ boolJson step.serverRoutePresent ++ ","
    ++ "\"admin_pin_present\":" ++ boolJson step.adminPinPresent ++ ","
    ++ "\"admin_pin_conflict\":" ++ boolJson step.adminPinConflict ++ ","
    ++ "\"challenge_binding_conflict\":" ++ boolJson step.challengeBindingConflict ++ ","
    ++ "\"request_binding_conflict\":" ++ boolJson step.requestBindingConflict ++ ","
    ++ "\"current_approval\":" ++ boolJson step.currentApproval ++ ","
    ++ "\"peer_admitted\":" ++ boolJson step.peerAdmitted ++ ","
    ++ "\"ready\":" ++ boolJson step.ready
    ++ "}"

def enrollmentCaseJson (c : EnrollmentCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"steps\":" ++ jsonArray (c.steps.map enrollmentTraceStepJson)
    ++ "}"

def enrollmentDurableProjectionCaseJson (c : EnrollmentDurableProjectionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"documents\":" ++ jsonArray (c.documents.map enrollmentTraceStepJson) ++ ","
    ++ "\"expected_current_approval\":" ++ boolJson c.expectedCurrentApproval ++ ","
    ++ "\"expected_current_route_receipt\":" ++ boolJson c.expectedCurrentRouteReceipt
    ++ "}"

def enrollmentEncodingCaseJson (c : EnrollmentEncodingCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"value\":" ++ jsonString c.value ++ ","
    ++ "\"expected_frame\":" ++ jsonString c.expectedFrame ++ ","
    ++ "\"actual_frame\":" ++ jsonString c.actualFrame ++ ","
    ++ "\"frame_matches\":" ++ boolJson c.frameMatches
    ++ "}"

def enrollmentDigestCaseJson (c : EnrollmentDigestCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"fields\":" ++ jsonStringArray c.fields ++ ","
    ++ "\"expected_payload\":" ++ jsonString c.expectedPayload ++ ","
    ++ "\"actual_payload\":" ++ jsonString c.actualPayload ++ ","
    ++ "\"expected_digest\":" ++ jsonString c.expectedDigest ++ ","
    ++ "\"actual_digest\":" ++ jsonString c.actualDigest ++ ","
    ++ "\"payload_matches\":" ++ boolJson c.payloadMatches ++ ","
    ++ "\"digest_matches\":" ++ boolJson c.digestMatches
    ++ "}"

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

def enrollmentCasesJson : String := jsonArray (enrollmentCases.map enrollmentCaseJson)
def enrollmentDurableProjectionCasesJson : String :=
  jsonArray (enrollmentDurableProjectionCases.map enrollmentDurableProjectionCaseJson)
def enrollmentEncodingCasesJson : String :=
  jsonArray (enrollmentEncodingCases.map enrollmentEncodingCaseJson)
def enrollmentDigestCasesJson : String :=
  jsonArray (enrollmentDigestCases.map enrollmentDigestCaseJson)
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

def titlePurposeWireCasesJson : String :=
  jsonArray (titlePurposeWireCases.map fun case =>
    "{" ++ "\"name\":" ++ jsonString case.name ++ ","
      ++ "\"wire\":" ++ jsonOptionalString case.wire ++ ","
      ++ "\"expected_decoded\":" ++
        jsonOptionalString (case.expectedDecoded.map RequestPurpose.toWire) ++ "}")

end Conformance.Contracts
