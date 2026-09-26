import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Contracts.Json.Core

namespace Conformance.Contracts

open Conformance.ContractCases

private def boolJson (value : Bool) : String := if value then "true" else "false"

def enrollmentTraceStepJson (step : EnrollmentTraceStep) : String :=
  "{"
    ++ "\"action\":" ++ jsonString step.action ++ ","
    ++ "\"peer_admission_did\":" ++ jsonString step.peerAdmissionDid ++ ","
    ++ "\"offer_id\":" ++ jsonString step.offerId ++ ","
    ++ "\"offer_challenge\":" ++ jsonString step.offerChallenge ++ ","
    ++ "\"offer_network_id\":" ++ jsonString step.offerNetworkId ++ ","
    ++ "\"offer_admin_did\":" ++ jsonString step.offerAdminDid ++ ","
    ++ "\"offer_server_peer\":" ++ jsonString step.offerServerPeer ++ ","
    ++ "\"offer_owner_agent\":" ++ jsonString step.offerOwnerAgent ++ ","
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
    ++ "\"owner_agent\":" ++ jsonString step.ownerAgent ++ ","
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
    ++ "\"decision_owner_agent\":" ++ jsonString step.decisionOwnerAgent ++ ","
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
    ++ "\"revision_owner_agent\":" ++ jsonString step.revisionOwnerAgent ++ ","
    ++ "\"revision_admin_signed\":" ++ boolJson step.revisionAdminSigned ++ ","
    ++ "\"receipt_request_id\":" ++ jsonString step.receiptRequestId ++ ","
    ++ "\"receipt_request_digest\":" ++ jsonString step.receiptRequestDigest ++ ","
    ++ "\"receipt_network_id\":" ++ jsonString step.receiptNetworkId ++ ","
    ++ "\"receipt_admin_did\":" ++ jsonString step.receiptAdminDid ++ ","
    ++ "\"receipt_member_did\":" ++ jsonString step.receiptMemberDid ++ ","
    ++ "\"receipt_member_peer\":" ++ jsonString step.receiptMemberPeer ++ ","
    ++ "\"receipt_server_peer\":" ++ jsonString step.receiptServerPeer ++ ","
    ++ "\"receipt_owner_agent\":" ++ jsonString step.receiptOwnerAgent ++ ","
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
    ++ "\"ready\":" ++ boolJson step.ready ++ ","
    ++ "\"client_hydration_admits\":" ++ boolJson step.clientHydrationAdmits ++ ","
    ++ "\"server_hydration_admits\":" ++ boolJson step.serverHydrationAdmits
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
    ++ "\"requester_matches_issuer\":" ++ boolJson c.requesterMatchesIssuer ++ ","
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
    ++ "\"peer_authority_allows\":" ++ boolJson c.peerAuthorityAllows ++ ","
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

private def titleFieldsHexJson (fields : Enrollment.CanonicalFields) : String :=
  jsonStringArray (fields.map Enrollment.utf8HexString)

private def titleRequestInputJson (input : Enrollment.RequestInput) : String :=
  "{" ++ "\"selected_skill_ids\":" ++ jsonStringArray input.selectedSkillIds ++ ","
    ++ "\"cwd\":" ++ jsonOptionalString input.cwd ++ ","
    ++ "\"initial_title\":" ++ (match input.initialTitle with
      | none => "null"
      | some title => "{\"text\":" ++ jsonString title.text ++
          ",\"source\":" ++ jsonString title.source.toWireName ++ "}") ++ ","
    ++ "\"queue\":" ++ (match input.queue with
      | none => "null"
      | some queue => "{\"source\":" ++ jsonString queue.source.toDefraDB ++
          ",\"policy\":" ++ jsonString queue.policy.toDefraDB ++
          ",\"key\":" ++ jsonOptionalString queue.key ++
          ",\"queued_after_request_id\":" ++
            jsonOptionalString queue.queuedAfterRequestId ++
          ",\"interrupted_request_id\":" ++
            jsonOptionalString queue.interruptedRequestId ++
          ",\"background_completion_wake_version\":" ++
            jsonOptionalNat queue.backgroundCompletionWakeVersion ++ "}") ++ ","
    ++ "\"goal_continuation\":" ++ (match input.goalContinuation with
      | none => "null"
      | some goal => "{\"sequence\":" ++ toString goal.sequence ++
          ",\"wrapup\":" ++ boolJson goal.wrapup ++ "}") ++ "}"

private def titleParentEvidenceJson (parent : Enrollment.TitleParentEvidence) : String :=
  "{" ++ "\"request_id\":" ++ jsonString parent.link.requestId ++ ","
    ++ "\"document_id\":" ++ jsonString parent.link.documentId ++ ","
    ++ "\"agent_did\":" ++ jsonString parent.agentDid ++ ","
    ++ "\"session_id\":" ++ jsonString parent.sessionId ++ ","
    ++ "\"behavior_id\":" ++ jsonString parent.behaviorId ++ ","
    ++ "\"logical_binding_current\":" ++ boolJson parent.logicalBindingCurrent ++ ","
    ++ "\"physical_binding_current\":" ++ boolJson parent.physicalBindingCurrent ++ "}"

private def titleRuntimeEvidenceJson (evidence : Enrollment.RuntimeInternalEvidence) : String :=
  "{" ++ "\"source_kind\":" ++ jsonString (match evidence.sourceKind with
      | .localControl => "local-control"
      | .automatedTrigger => "automated-trigger") ++ ","
    ++ "\"issuer_did\":" ++ jsonString evidence.issuerDid ++ ","
    ++ "\"source_request_id\":" ++ jsonString evidence.sourceRequestId ++ ","
    ++ "\"target_agent\":" ++ jsonString evidence.targetAgent ++ ","
    ++ "\"target_runtime_attestation_valid\":" ++ boolJson evidence.targetRuntimeAttestationValid ++ ","
    ++ "\"source_binding_current\":" ++ boolJson evidence.sourceBindingCurrent ++ ","
    ++ "\"source_document_binding_current\":" ++ boolJson evidence.sourceDocumentBindingCurrent ++ ","
    ++ "\"title_parent\":" ++
      (match evidence.titleParent with
      | none => "null"
      | some parent => titleParentEvidenceJson parent) ++ "}"

def titleRequestAdmissionCaseJson (case : TitleRequestAdmissionCase) : String :=
  let request := case.request
  let admission := case.admission
  "{" ++ "\"name\":" ++ jsonString case.name ++ ","
    ++ "\"parent_observed_state\":" ++ jsonString case.parentObservedState.toDefraDB ++ ","
    ++ "\"observation_available\":" ++ boolJson case.observationAvailable ++ ","
    ++ "\"branch_fields_exact\":" ++ boolJson case.branchFieldsExact ++ ","
    ++ "\"pending_deadline_absent\":" ++ boolJson case.pendingDeadlineAbsent ++ ","
    ++ "\"request\":{"
    ++ "\"request_id\":" ++ jsonString request.requestId ++ ","
    ++ "\"purpose\":" ++ jsonString request.purpose.toWire ++ ","
    ++ "\"target_agent\":" ++ jsonString request.targetAgent ++ ","
    ++ "\"requester_did\":" ++ jsonString request.requesterDid ++ ","
    ++ "\"behavior_id\":" ++ jsonString request.behaviorId ++ ","
    ++ "\"session_id\":" ++ jsonString request.sessionId ++ ","
    ++ "\"content\":" ++ jsonString request.content ++ ","
    ++ "\"input\":" ++ titleRequestInputJson request.input ++ ","
    ++ "\"model_input_fields_hex\":" ++ titleFieldsHexJson (Enrollment.requestInputFields request.input) ++ ","
    ++ "\"created_at\":" ++ jsonString request.createdAt ++ ","
    ++ "\"trigger_config_document_id\":" ++ jsonString request.triggerConfigDocumentId ++ ","
    ++ "\"model_retry_fields_hex\":" ++ titleFieldsHexJson request.retryFields ++ ","
    ++ "\"model_trigger_fields_hex\":" ++ titleFieldsHexJson request.triggerFields ++ ","
    ++ "\"model_parent_fields_hex\":" ++ titleFieldsHexJson request.parentFields ++ ","
    ++ "\"model_workspace_fields_hex\":" ++
      titleFieldsHexJson (Enrollment.requestWorkspaceFields request.workspace) ++ ","
    ++ "\"model_semantic_fields_hex\":" ++
      titleFieldsHexJson (Enrollment.agentRequestSemanticFields request) ++ "},"
    ++ "\"admission\":{"
    ++ "\"kind\":" ++ jsonString (match admission.kind with
      | .enrollment => "enrollment"
      | .localSelf => "local-self"
      | .runtimeInternal => "runtime-internal"
      | .peer => "peer") ++ ","
    ++ "\"signer_did\":" ++ jsonString admission.signerDid ++ ","
    ++ "\"issuer_did\":" ++ jsonString admission.issuerDid ++ ","
    ++ "\"source_request_id\":" ++ jsonString admission.sourceRequestId ++ ","
    ++ "\"runtime_source_kind\":" ++ jsonString (match admission.runtimeSourceKind with
      | .localControl => "local-control"
      | .automatedTrigger => "automated-trigger") ++ ","
    ++ "\"signature_valid\":" ++ boolJson admission.signatureValid ++ ","
    ++ "\"model_signed_fields_hex\":" ++ titleFieldsHexJson admission.signedFields ++ ","
    ++ "\"model_expected_fields_hex\":" ++
      titleFieldsHexJson (Enrollment.agentRequestAdmissionFields request admission) ++ "},"
    ++ "\"runtime_evidence\":" ++ (match case.runtimeEvidence with
      | none => "null"
      | some evidence => titleRuntimeEvidenceJson evidence) ++ ","
    ++ "\"session_behavior\":" ++ jsonString case.sessionBehavior ++ ","
    ++ "\"expected_admitted\":" ++ boolJson case.expectedAdmitted ++ ","
    ++ "\"expected_claimable\":" ++ boolJson case.expectedClaimable ++ ","
    ++ "\"expected_disposition\":" ++ jsonString (match case.expectedDisposition with
      | .admit => "admit" | .deny => "deny" | .retry => "retry") ++ ","
    ++ "\"expected_pending_state\":" ++
      jsonOptionalString (case.expectedPendingState.map (·.toDefraDB)) ++ "}"

def titleRequestAdmissionCasesJson : String :=
  jsonArray (titleRequestAdmissionCases.map titleRequestAdmissionCaseJson)

def titlePurposeWireCasesJson : String :=
  jsonArray (titlePurposeWireCases.map fun case =>
    "{" ++ "\"name\":" ++ jsonString case.name ++ ","
      ++ "\"wire\":" ++ jsonOptionalString case.wire ++ ","
      ++ "\"expected_decoded\":" ++
        jsonOptionalString (case.expectedDecoded.map RequestPurpose.toWire) ++ "}")

end Conformance.Contracts
