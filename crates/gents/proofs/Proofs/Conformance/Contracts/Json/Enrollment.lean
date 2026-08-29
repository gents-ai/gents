import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Contracts.Json.Core

namespace Conformance.Contracts

open Conformance.ContractCases

private def boolJson (value : Bool) : String := if value then "true" else "false"

def enrollmentTraceStepJson (step : EnrollmentTraceStep) : String :=
  "{"
    ++ "\"action\":" ++ jsonString step.action ++ ","
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
    ++ "\"revision_signer_did\":" ++ jsonString step.revisionSignerDid ++ ","
    ++ "\"revision_request_id\":" ++ jsonString step.revisionRequestId ++ ","
    ++ "\"revision_request_digest\":" ++ jsonString step.revisionRequestDigest ++ ","
    ++ "\"revision_network_id\":" ++ jsonString step.revisionNetworkId ++ ","
    ++ "\"revision_admin_did\":" ++ jsonString step.revisionAdminDid ++ ","
    ++ "\"revision_member_did\":" ++ jsonString step.revisionMemberDid ++ ","
    ++ "\"revision_member_peer\":" ++ jsonString step.revisionMemberPeer ++ ","
    ++ "\"revision_owner_agent\":" ++ jsonString step.revisionOwnerAgent ++ ","
    ++ "\"revision_admin_signed\":" ++ boolJson step.revisionAdminSigned ++ ","
    ++ "\"observed_offer_count\":" ++ toString step.observedOfferCount ++ ","
    ++ "\"admin_pin_count\":" ++ toString step.adminPinCount ++ ","
    ++ "\"challenge_binding_count\":" ++ toString step.challengeBindingCount ++ ","
    ++ "\"request_binding_count\":" ++ toString step.requestBindingCount ++ ","
    ++ "\"request_count\":" ++ toString step.requestCount ++ ","
    ++ "\"decision_count\":" ++ toString step.decisionCount ++ ","
    ++ "\"authorization_count\":" ++ toString step.authorizationCount ++ ","
    ++ "\"membership_count\":" ++ toString step.membershipCount ++ ","
    ++ "\"route_count\":" ++ toString step.routeCount ++ ","
    ++ "\"request_accepted\":" ++ boolJson step.requestAccepted ++ ","
    ++ "\"decision_recorded\":" ++ boolJson step.decisionRecorded ++ ","
    ++ "\"authorization_recorded\":" ++ boolJson step.authorizationRecorded ++ ","
    ++ "\"revision_recorded\":" ++ boolJson step.revisionRecorded ++ ","
    ++ "\"membership_present\":" ++ boolJson step.membershipPresent ++ ","
    ++ "\"client_route_present\":" ++ boolJson step.clientRoutePresent ++ ","
    ++ "\"server_route_present\":" ++ boolJson step.serverRoutePresent ++ ","
    ++ "\"admin_pin_present\":" ++ boolJson step.adminPinPresent ++ ","
    ++ "\"admin_pin_conflict\":" ++ boolJson step.adminPinConflict ++ ","
    ++ "\"challenge_binding_conflict\":" ++ boolJson step.challengeBindingConflict ++ ","
    ++ "\"request_binding_conflict\":" ++ boolJson step.requestBindingConflict ++ ","
    ++ "\"current_approval\":" ++ boolJson step.currentApproval ++ ","
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
    ++ "\"expected_current_approval\":" ++ boolJson c.expectedCurrentApproval
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

def enrollmentCasesJson : String := jsonArray (enrollmentCases.map enrollmentCaseJson)
def enrollmentDurableProjectionCasesJson : String :=
  jsonArray (enrollmentDurableProjectionCases.map enrollmentDurableProjectionCaseJson)
def enrollmentEncodingCasesJson : String :=
  jsonArray (enrollmentEncodingCases.map enrollmentEncodingCaseJson)
def enrollmentDigestCasesJson : String :=
  jsonArray (enrollmentDigestCases.map enrollmentDigestCaseJson)

end Conformance.Contracts
