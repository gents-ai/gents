import Proofs.Enrollment
import Proofs.Enrollment.RequestAdmission
import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases

open Enrollment

private def requestAdmissionKindName : AgentRequestAdmissionKind → String
  | .enrollment => "enrollment"
  | .localSelf => "local-self"
  | .runtimeInternal => "runtime-internal"

private def runtimeInternalSourceKindName : RuntimeInternalSourceKind → String
  | .localChild => "local-child"
  | .crossDeploymentChild => "cross-deployment-child"
  | .localControl => "local-control"
  | .automatedTrigger => "automated-trigger"

private def requestAdmissionDispositionName : AgentRequestAdmissionDisposition → String
  | .admit => "admit"
  | .deny => "deny"
  | .retry => "retry"

private def requestAdmissionCase (name : String)
    (observation : AgentRequestAdmissionObservation)
    (observationAvailable : Bool := true) : AgentRequestAdmissionCase :=
  { name
  , observationAvailable
  , kind := requestAdmissionKindName observation.kind
  , signatureValid := observation.signatureValid
  , signedFieldsMatch := observation.signedFieldsMatch
  , branchFieldsExact := observation.branchFieldsExact
  , pendingDeadlineAbsent := observation.pendingDeadlineAbsent
  , signerMatchesRequester := observation.signerMatchesRequester
  , requesterMatchesTarget := observation.requesterMatchesTarget
  , signerMatchesTarget := observation.signerMatchesTarget
  , signerMatchesIssuer := observation.signerMatchesIssuer
  , requesterMatchesIssuer := observation.requesterMatchesIssuer
  , currentApproval := observation.currentApproval
  , exactGeneration := observation.exactGeneration
  , authorizationFresh := observation.authorizationFresh
  , runtimeEvidencePresent := observation.runtimeEvidencePresent
  , runtimeSourceKind := runtimeInternalSourceKindName observation.runtimeSourceKind
  , targetRuntimeAttestationValid := observation.targetRuntimeAttestationValid
  , sourceBindingCurrent := observation.sourceBindingCurrent
  , triggerConfigDocumentBindingCurrent := observation.triggerConfigDocumentBindingCurrent
  , sourceDocumentBindingCurrent := observation.sourceDocumentBindingCurrent
  , sourceToolCallBindingCurrent := observation.sourceToolCallBindingCurrent
  , targetPolicyAllows := observation.targetPolicyAllows
  , bridgeAuthorBindingCurrent := observation.bridgeAuthorBindingCurrent
  , bridgeAuthorAuthorizationFresh := observation.bridgeAuthorAuthorizationFresh
  , targetCrossDeploymentPolicyAllows := observation.targetCrossDeploymentPolicyAllows
  , expectedAdmitted := projectAgentRequestAdmission observation
  , expectedDisposition := requestAdmissionDispositionName
      (projectAgentRequestAdmissionDisposition observationAvailable observation) }

private def requestAdmissionBase (kind : AgentRequestAdmissionKind) :
    AgentRequestAdmissionObservation :=
  { kind
  , signatureValid := true
  , signedFieldsMatch := true
  , branchFieldsExact := true
  , pendingDeadlineAbsent := true
  , signerMatchesRequester := true
  , requesterMatchesTarget := true
  , signerMatchesTarget := true
  , signerMatchesIssuer := true
  , requesterMatchesIssuer := true
  , currentApproval := true
  , exactGeneration := true
  , authorizationFresh := true
  , runtimeEvidencePresent := false
  , runtimeSourceKind := .localControl
  , targetRuntimeAttestationValid := false
  , sourceBindingCurrent := false
  , triggerConfigDocumentBindingCurrent := false
  , sourceDocumentBindingCurrent := false
  , sourceToolCallBindingCurrent := false
  , targetPolicyAllows := false
  , bridgeAuthorBindingCurrent := false
  , bridgeAuthorAuthorizationFresh := false
  , targetCrossDeploymentPolicyAllows := false }

def agentRequestAdmissionCases : List AgentRequestAdmissionCase :=
  [ requestAdmissionCase "valid-current-enrollment" (requestAdmissionBase .enrollment)
  , requestAdmissionCase "revoked-enrollment"
      { requestAdmissionBase .enrollment with currentApproval := false }
  , requestAdmissionCase "expired-enrollment"
      { requestAdmissionBase .enrollment with authorizationFresh := false }
  , requestAdmissionCase "superseded-enrollment-generation"
      { requestAdmissionBase .enrollment with exactGeneration := false }
  , requestAdmissionCase "forged-enrollment-signature"
      { requestAdmissionBase .enrollment with signatureValid := false }
  , requestAdmissionCase "valid-signed-local-self" (requestAdmissionBase .localSelf)
  , requestAdmissionCase "forged-local-self"
      { requestAdmissionBase .localSelf with signerMatchesRequester := false }
  , requestAdmissionCase "valid-runtime-local-control"
      { requestAdmissionBase .runtimeInternal with
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, sourceDocumentBindingCurrent := true }
  , requestAdmissionCase "valid-runtime-local-child"
      { requestAdmissionBase .runtimeInternal with
          runtimeSourceKind := .localChild
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, sourceDocumentBindingCurrent := true
          sourceToolCallBindingCurrent := true, targetPolicyAllows := true }
  , requestAdmissionCase "valid-runtime-cross-deployment-child-without-parent-document"
      { requestAdmissionBase .runtimeInternal with
          runtimeSourceKind := .crossDeploymentChild
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, sourceDocumentBindingCurrent := false
          sourceToolCallBindingCurrent := true, bridgeAuthorBindingCurrent := true
          bridgeAuthorAuthorizationFresh := true
          targetCrossDeploymentPolicyAllows := true }
  , requestAdmissionCase "valid-runtime-automated-trigger"
      { requestAdmissionBase .runtimeInternal with
          runtimeSourceKind := .automatedTrigger
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, triggerConfigDocumentBindingCurrent := true
          targetPolicyAllows := true }
  , requestAdmissionCase "lineage-alone-cannot-select-runtime-internal"
      { requestAdmissionBase .runtimeInternal with
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := false
          sourceBindingCurrent := true, sourceDocumentBindingCurrent := true }
  , requestAdmissionCase "stale-runtime-source-binding"
      { requestAdmissionBase .runtimeInternal with
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := false, sourceDocumentBindingCurrent := true }
  , requestAdmissionCase "stale-runtime-trigger-config-document"
      { requestAdmissionBase .runtimeInternal with
          runtimeSourceKind := .automatedTrigger
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, triggerConfigDocumentBindingCurrent := false
          sourceDocumentBindingCurrent := true
          sourceToolCallBindingCurrent := true, targetPolicyAllows := true }
  , requestAdmissionCase "stale-runtime-source-document"
      { requestAdmissionBase .runtimeInternal with
          runtimeSourceKind := .localChild
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, triggerConfigDocumentBindingCurrent := true
          sourceDocumentBindingCurrent := false
          sourceToolCallBindingCurrent := true, targetPolicyAllows := true }
  , requestAdmissionCase "stale-runtime-tool-call-document"
      { requestAdmissionBase .runtimeInternal with
          runtimeSourceKind := .localChild
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, triggerConfigDocumentBindingCurrent := true
          sourceDocumentBindingCurrent := true
          sourceToolCallBindingCurrent := false, targetPolicyAllows := true }
  , requestAdmissionCase "runtime-target-policy-denied"
      { requestAdmissionBase .runtimeInternal with
          runtimeSourceKind := .localChild
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, triggerConfigDocumentBindingCurrent := true
          sourceDocumentBindingCurrent := true
          sourceToolCallBindingCurrent := true, targetPolicyAllows := false }
  , requestAdmissionCase "cross-deployment-author-revoked"
      { requestAdmissionBase .runtimeInternal with
          runtimeSourceKind := .crossDeploymentChild
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, sourceToolCallBindingCurrent := true
          bridgeAuthorBindingCurrent := true
          bridgeAuthorAuthorizationFresh := false
          targetCrossDeploymentPolicyAllows := true }
  , requestAdmissionCase "cross-deployment-target-policy-denied"
      { requestAdmissionBase .runtimeInternal with
          runtimeSourceKind := .crossDeploymentChild
          runtimeEvidencePresent := true, targetRuntimeAttestationValid := true
          sourceBindingCurrent := true, sourceToolCallBindingCurrent := true
          bridgeAuthorBindingCurrent := true
          bridgeAuthorAuthorizationFresh := true
          targetCrossDeploymentPolicyAllows := false }
  , requestAdmissionCase "cross-target-local-self"
      { requestAdmissionBase .localSelf with requesterMatchesTarget := false }
  , requestAdmissionCase "tampered-signed-request-fields"
      { requestAdmissionBase .enrollment with signedFieldsMatch := false }
  , requestAdmissionCase "missing-or-unknown-execution-origin"
      { requestAdmissionBase .enrollment with signedFieldsMatch := false }
  , requestAdmissionCase "caller-authored-preclaim-deadline"
      { requestAdmissionBase .enrollment with pendingDeadlineAbsent := false }
  , requestAdmissionCase "enrollment-with-foreign-branch-fields"
      { requestAdmissionBase .enrollment with branchFieldsExact := false }
  , requestAdmissionCase "local-self-with-foreign-branch-fields"
      { requestAdmissionBase .localSelf with branchFieldsExact := false }
  , requestAdmissionCase "runtime-internal-with-foreign-branch-fields"
      { requestAdmissionBase .runtimeInternal with
          branchFieldsExact := false, runtimeEvidencePresent := true
          targetRuntimeAttestationValid := true, sourceBindingCurrent := true
          sourceDocumentBindingCurrent := true }
  , requestAdmissionCase "authority-observation-unavailable-retries"
      (requestAdmissionBase .enrollment) false
  ]

open Enrollment

def enrollmentOffer : Offer :=
  { offerId := "offer-1", challenge := "challenge-1"
  , networkId := "network-1", adminDid := "did:key:admin"
  , serverPeer := "server-peer", serverTicketPeer := "server-peer"
  , resolvedServerDid := "did:key:admin", ownerAgent := "did:key:agent"
  , profile := "client", schemaCompatible := true, adminSigned := true, fresh := true }

def unsignedEnrollmentRequest : Request :=
  { requestId := "request-1", digest := emptyDigest
  , offerId := "offer-1", challenge := "challenge-1"
  , networkId := "network-1", adminDid := "did:key:admin"
  , serverPeer := "server-peer", candidateDid := "did:key:candidate"
  , candidatePeer := "candidate-peer", observedCandidatePeer := "candidate-peer"
  , resolvedCandidateDid := "did:key:candidate", candidateTicketPeer := "candidate-peer"
  , ownerAgent := "did:key:agent", profile := "client"
  , clientNonce := "nonce-1", issuedAt := "1", expiresAt := "2"
  , candidateSigned := true, fresh := true }

def canonicalizeEnrollmentRequest (r : Request) : Request :=
  { r with digest := canonicalRequestDigest r }

def enrollmentRequest : Request := canonicalizeEnrollmentRequest unsignedEnrollmentRequest

def decisionFor (r : Request) (kind : DecisionKind := .approved) (sequence : Nat := 1) : Decision :=
  { requestId := r.requestId, requestDigest := r.digest
  , networkId := r.networkId, adminDid := r.adminDid
  , candidateDid := r.candidateDid, candidatePeer := r.candidatePeer
  , ownerAgent := r.ownerAgent, kind, authorizationSequence := sequence
  , authorizationExpiresAt := "lease-2"
  , signerDid := r.adminDid, adminSigned := true, fresh := true }

def revocationFor (r : Request) (sequence : Nat := 2) : AuthorizationRevision :=
  { requestId := r.requestId, requestDigest := r.digest
  , networkId := r.networkId, adminDid := r.adminDid
  , memberDid := r.candidateDid, memberPeer := r.candidatePeer
  , ownerAgent := r.ownerAgent, sequence, kind := .revoked
  , authorizationExpiresAt := "lease-2"
  , signerDid := r.adminDid, adminSigned := true }

def enrollmentDecision : Decision := decisionFor enrollmentRequest
def enrollmentRevocation : AuthorizationRevision := revocationFor enrollmentRequest
def enrollmentRouteReceipt : RouteReceipt :=
  serverRouteReceiptFor enrollmentRequest enrollmentDecision

inductive EnrollmentAction where
  | observe (offer : Offer)
  | confirmPin (offer : Offer)
  | accept (offer : Offer) (request : Request)
  | decide (request : Request) (decision : Decision)
  | membership (request : Request) (decision : Decision)
  | clientRoute (request : Request) (decision : Decision)
  | serverReceipt (request : Request) (decision : Decision) (receipt : RouteReceipt)
  | revoke (request : Request) (revision : AuthorizationRevision)
  /-- A restore/replication merge, with the approval whose projection is observed. -/
  | merge (request : Request) (decision : Decision) (revision : AuthorizationRevision)
  deriving Repr

def applyEnrollmentAction (state : Enrollment.State) : EnrollmentAction → Enrollment.State
  | .observe offer => observeOffer state offer
  | .confirmPin offer => confirmAdminPin state offer
  | .accept offer request => acceptRequest state offer request
  | .decide request decision => decideRequest state request decision
  | .membership request decision => materializeMembership state request decision
  | .clientRoute request decision => materializeClientRoute state request decision
  | .serverReceipt request decision receipt =>
      recordServerRouteReceipt state request decision receipt
  | .revoke request revision => Enrollment.revoke state request revision
  | .merge _ _ revision => mergeAuthorization state revision

private def actionName : EnrollmentAction → String
  | .observe _ => "observe_offer"
  | .confirmPin _ => "confirm_admin_pin"
  | .accept _ _ => "accept_request"
  | .decide _ decision => match decision.kind with
      | .approved => "approve_request" | .denied => "deny_request"
  | .membership _ _ => "materialize_membership"
  | .clientRoute _ _ => "materialize_client_route"
  | .serverReceipt _ _ _ => "record_server_route_receipt"
  | .revoke _ _ => "revoke_membership"
  | .merge _ _ _ => "merge_authorization"

private def actionOffer? : EnrollmentAction → Option Offer
  | .observe offer | .confirmPin offer | .accept offer _ => some offer
  | _ => none

private def actionRequest? : EnrollmentAction → Option Request
  | .accept _ request | .decide request _ | .membership request _
  | .clientRoute request _ | .serverReceipt request _ _
  | .revoke request _ | .merge request _ _ => some request
  | _ => none

private def actionDecision? : EnrollmentAction → Option Decision
  | .decide _ decision | .membership _ decision | .clientRoute _ decision
  | .serverReceipt _ decision _
  | .merge _ decision _ => some decision
  | _ => none

private def actionRevision? : EnrollmentAction → Option AuthorizationRevision
  | .revoke _ revision | .merge _ _ revision => some revision
  | _ => none

private def actionReceipt? : EnrollmentAction → Option RouteReceipt
  | .serverReceipt _ _ receipt => some receipt
  | _ => none

private def actionPeerAdmissionDid : EnrollmentAction → Did
  | action => actionRequest? action |>.map Request.candidateDid |>.getD ""

private def routeDirectionName : RouteDirection → String
  | .clientToServer => "client_to_server"
  | .serverToClient => "server_to_client"

private def decisionKindName : DecisionKind → String
  | .approved => "approved" | .denied => "denied"

private def revisionKindName : RevisionKind → String
  | .active => "active" | .revoked => "revoked"

def enrollmentTraceStep (action : EnrollmentAction) (state : Enrollment.State) :
    EnrollmentTraceStep :=
  let offer? := actionOffer? action
  let request? := actionRequest? action
  let decision? := actionDecision? action
  let revision? := actionRevision? action
  let receipt? := actionReceipt? action
  let peerAdmissionDid := actionPeerAdmissionDid action
  let offerId := match offer? with | some offer => offer.offerId | none => ""
  let offerChallenge := match offer? with | some offer => offer.challenge | none => ""
  let offerNetworkId := match offer? with | some offer => offer.networkId | none => ""
  let offerAdminDid := match offer? with | some offer => offer.adminDid | none => ""
  let offerServerPeer := match offer? with | some offer => offer.serverPeer | none => ""
  let offerOwnerAgent := match offer? with | some offer => offer.ownerAgent | none => ""
  let offerProfile := match offer? with | some offer => offer.profile | none => ""
  let challenge := match request?, offer? with
    | some request, _ => request.challenge
    | none, some offer => offer.challenge
    | none, none => ""
  let requestId := match request? with | some request => request.requestId | none => ""
  let requestDigest := match request? with
    | some request => renderDigestString request.digest | none => ""
  let requestOfferId := match request? with | some request => request.offerId | none => ""
  let networkId := match request?, offer? with
    | some request, _ => request.networkId
    | none, some offer => offer.networkId
    | none, none => ""
  let adminDid := match request?, offer? with
    | some request, _ => request.adminDid
    | none, some offer => offer.adminDid
    | none, none => ""
  let serverPeer := match request?, offer? with
    | some request, _ => request.serverPeer
    | none, some offer => offer.serverPeer
    | none, none => ""
  let serverTicketPeer := match offer? with
    | some offer => offer.serverTicketPeer | none => ""
  let resolvedServerDid := match offer? with
    | some offer => offer.resolvedServerDid | none => ""
  let profile := match request?, offer? with
    | some request, _ => request.profile
    | none, some offer => offer.profile
    | none, none => ""
  let schemaCompatible := match offer? with
    | some offer => offer.schemaCompatible | none => false
  let offerAdminSigned := match offer? with
    | some offer => offer.adminSigned | none => false
  let offerFresh := match offer? with | some offer => offer.fresh | none => false
  let candidateDid := match request? with | some request => request.candidateDid | none => ""
  let candidatePeer := match request? with | some request => request.candidatePeer | none => ""
  let observedCandidatePeer := match request? with
    | some request => request.observedCandidatePeer | none => ""
  let resolvedCandidateDid := match request? with
    | some request => request.resolvedCandidateDid | none => ""
  let candidateTicketPeer := match request? with
    | some request => request.candidateTicketPeer | none => ""
  let ownerAgent := match request?, offer? with
    | some request, _ => request.ownerAgent
    | none, some offer => offer.ownerAgent
    | none, none => ""
  let clientNonce := match request? with | some request => request.clientNonce | none => ""
  let issuedAt := match request? with | some request => request.issuedAt | none => ""
  let expiresAt := match request? with | some request => request.expiresAt | none => ""
  let candidateSigned := match request? with
    | some request => request.candidateSigned | none => false
  let requestFresh := match request? with | some request => request.fresh | none => false
  let decisionAuthorizationSequence := match decision? with
    | some decision => decision.authorizationSequence | none => 0
  let decisionAuthorizationExpiresAt := match decision? with
    | some decision => decision.authorizationExpiresAt | none => ""
  let decisionSignerDid := match decision? with
    | some decision => decision.signerDid | none => ""
  let decisionKind := match decision? with
    | some decision => decisionKindName decision.kind | none => ""
  let decisionRequestId := match decision? with
    | some decision => decision.requestId | none => ""
  let decisionRequestDigest := match decision? with
    | some decision => renderDigestString decision.requestDigest | none => ""
  let decisionNetworkId := match decision? with
    | some decision => decision.networkId | none => ""
  let decisionAdminDid := match decision? with
    | some decision => decision.adminDid | none => ""
  let decisionCandidateDid := match decision? with
    | some decision => decision.candidateDid | none => ""
  let decisionCandidatePeer := match decision? with
    | some decision => decision.candidatePeer | none => ""
  let decisionOwnerAgent := match decision? with
    | some decision => decision.ownerAgent | none => ""
  let decisionAdminSigned := match decision? with
    | some decision => decision.adminSigned | none => false
  let decisionFresh := match decision? with
    | some decision => decision.fresh | none => false
  let revisionKind := match revision? with
    | some revision => revisionKindName revision.kind | none => ""
  let revisionSequence := match revision? with
    | some revision => revision.sequence | none => 0
  let revisionAuthorizationExpiresAt := match revision? with
    | some revision => revision.authorizationExpiresAt | none => ""
  let revisionSignerDid := match revision? with
    | some revision => revision.signerDid | none => ""
  let revisionRequestId := match revision? with
    | some revision => revision.requestId | none => ""
  let revisionRequestDigest := match revision? with
    | some revision => renderDigestString revision.requestDigest | none => ""
  let revisionNetworkId := match revision? with
    | some revision => revision.networkId | none => ""
  let revisionAdminDid := match revision? with
    | some revision => revision.adminDid | none => ""
  let revisionMemberDid := match revision? with
    | some revision => revision.memberDid | none => ""
  let revisionMemberPeer := match revision? with
    | some revision => revision.memberPeer | none => ""
  let revisionOwnerAgent := match revision? with
    | some revision => revision.ownerAgent | none => ""
  let revisionAdminSigned := match revision? with
    | some revision => revision.adminSigned | none => false
  let receiptRequestId := match receipt? with | some receipt => receipt.requestId | none => ""
  let receiptRequestDigest := match receipt? with
    | some receipt => renderDigestString receipt.requestDigest | none => ""
  let receiptNetworkId := match receipt? with | some receipt => receipt.networkId | none => ""
  let receiptAdminDid := match receipt? with | some receipt => receipt.adminDid | none => ""
  let receiptMemberDid := match receipt? with | some receipt => receipt.memberDid | none => ""
  let receiptMemberPeer := match receipt? with | some receipt => receipt.memberPeer | none => ""
  let receiptServerPeer := match receipt? with | some receipt => receipt.serverPeer | none => ""
  let receiptOwnerAgent := match receipt? with | some receipt => receipt.ownerAgent | none => ""
  let receiptAuthorizationSequence := match receipt? with
    | some receipt => receipt.authorizationSequence | none => 0
  let receiptAuthorizationExpiresAt := match receipt? with
    | some receipt => receipt.authorizationExpiresAt | none => ""
  let receiptDirection := match receipt? with
    | some receipt => routeDirectionName receipt.direction | none => ""
  let receiptSignerDid := match receipt? with | some receipt => receipt.signerDid | none => ""
  let receiptAdminSigned := match receipt? with | some receipt => receipt.adminSigned | none => false
  let receiptApplied := match receipt? with | some receipt => receipt.applied | none => false
  let requestAccepted := match request? with
    | some request => decide (request ∈ state.acceptedRequests) | none => false
  let decisionRecorded := match decision? with
    | some decision => decide (decision ∈ state.decisions) | none => false
  let authorizationRecorded := match request?, decision? with
    | some request, some decision =>
        decide (revisionForApproval request decision ∈ state.authorizations)
    | _, _ => false
  let revisionRecorded := match revision? with
    | some revision => decide (revision ∈ state.authorizations)
    | none => false
  let receiptRecorded := match receipt? with
    | some receipt => decide (receipt ∈ state.routeReceipts)
    | none => false
  let membershipPresent := match request?, decision? with
    | some request, some decision => decide (membershipFor request decision ∈ state.memberships)
    | _, _ => false
  let clientRoutePresent := match request?, decision? with
    | some request, some decision =>
        decide (clientToServerRoute request decision ∈ state.appliedRoutes)
    | _, _ => false
  let serverRoutePresent := match request?, decision? with
    | some request, some decision =>
        decide (serverToClientRoute request decision ∈ state.appliedRoutes)
    | _, _ => false
  let current := match request?, decision? with
    | some request, some decision => decide (currentApproval state request decision)
    | _, _ => false
  let peerAdmitted := decide (peerOperationallyAuthorized state peerAdmissionDid)
  let ready := match request? with
    | some request => decide (enrollmentReady state request) | none => false
  let adminPinPresent := match offer? with
    | some offer => decide (adminPinFor offer ∈ state.adminPins) | none => false
  let pinConflict := match offer? with
    | some offer => decide (Enrollment.adminPinConflict state offer) | none => false
  let challengeBindingConflict := match request? with
    | some request => decide (challengeBoundElsewhere state request) | none => false
  let requestBindingConflict := match request? with
    | some request => decide (requestIdBoundElsewhere state request) | none => false
  let clientHydrationAdmits := match request? with
    | some request =>
        let hydration := hydrationRequestFor request "session-1"
        let sessions := {SessionHydration.ownedSession hydration}
        decide (SessionHydration.admits
          (projectedClientToServerHydrationCatalog state request.networkId sessions) hydration)
    | none => false
  let serverHydrationAdmits := match request? with
    | some request =>
        let hydration := reverseHydrationRequestFor request "session-1"
        let sessions := {SessionHydration.ownedSession hydration}
        decide (SessionHydration.admits
          (projectedServerToClientHydrationCatalog state request.networkId sessions) hydration)
    | none => false
  { action := actionName action, peerAdmissionDid, offerId, offerChallenge, offerNetworkId, offerAdminDid
  , offerServerPeer, offerOwnerAgent, offerProfile, challenge, requestId, requestDigest
  , requestOfferId
  , networkId, adminDid, serverPeer, serverTicketPeer, resolvedServerDid, profile
  , schemaCompatible, offerAdminSigned, offerFresh
  , candidateDid, candidatePeer, observedCandidatePeer, resolvedCandidateDid
  , candidateTicketPeer, ownerAgent, clientNonce, issuedAt, expiresAt
  , candidateSigned, requestFresh, decisionAuthorizationSequence
  , decisionAuthorizationExpiresAt, decisionSignerDid
  , decisionKind, decisionRequestId, decisionRequestDigest, decisionNetworkId, decisionAdminDid
  , decisionCandidateDid, decisionCandidatePeer, decisionOwnerAgent
  , decisionAdminSigned, decisionFresh, revisionKind, revisionSequence
  , revisionAuthorizationExpiresAt, revisionSignerDid
  , revisionRequestId, revisionRequestDigest
  , revisionNetworkId, revisionAdminDid, revisionMemberDid, revisionMemberPeer
  , revisionOwnerAgent, revisionAdminSigned
  , receiptRequestId, receiptRequestDigest, receiptNetworkId, receiptAdminDid
  , receiptMemberDid, receiptMemberPeer, receiptServerPeer, receiptOwnerAgent
  , receiptAuthorizationSequence, receiptAuthorizationExpiresAt
  , receiptDirection, receiptSignerDid
  , receiptAdminSigned, receiptApplied
  , observedOfferCount := state.observedOffers.card
  , adminPinCount := state.adminPins.card
  , challengeBindingCount := state.challengeBindings.card
  , requestBindingCount := state.requestBindings.card
  , requestCount := state.acceptedRequests.card
  , decisionCount := state.decisions.card
  , authorizationCount := state.authorizations.card
  , membershipCount := state.memberships.card
  , receiptCount := state.routeReceipts.card
  , routeCount := state.appliedRoutes.card
  , requestAccepted, decisionRecorded, authorizationRecorded, revisionRecorded, receiptRecorded
  , membershipPresent
  , clientRoutePresent, serverRoutePresent, adminPinPresent, adminPinConflict := pinConflict
  , challengeBindingConflict, requestBindingConflict
  , currentApproval := current, peerAdmitted, ready, clientHydrationAdmits, serverHydrationAdmits
  }

def runEnrollmentTrace : Enrollment.State → List EnrollmentAction → List EnrollmentTraceStep
  | _, [] => []
  | state, action :: rest =>
      let next := applyEnrollmentAction state action
      enrollmentTraceStep action next :: runEnrollmentTrace next rest

def enrollmentCase (name : String) (actions : List EnrollmentAction) : EnrollmentCase :=
  { name, steps := runEnrollmentTrace {} actions }

def validEnrollmentActions : List EnrollmentAction :=
  [.observe enrollmentOffer, .confirmPin enrollmentOffer,
   .accept enrollmentOffer enrollmentRequest,
   .decide enrollmentRequest enrollmentDecision,
   .membership enrollmentRequest enrollmentDecision,
   .clientRoute enrollmentRequest enrollmentDecision,
   .serverReceipt enrollmentRequest enrollmentDecision enrollmentRouteReceipt]

def invalidOfferCase (name : String) (offer : Offer) : EnrollmentCase :=
  enrollmentCase name [.observe offer, .confirmPin offer, .accept offer enrollmentRequest]

def invalidRequestCase (name : String) (request : Request) : EnrollmentCase :=
  enrollmentCase name [.observe enrollmentOffer, .confirmPin enrollmentOffer,
    .accept enrollmentOffer request]

def enrollmentCases : List EnrollmentCase :=
  let unsignedOffer := { enrollmentOffer with adminSigned := false }
  let expiredOffer := { enrollmentOffer with fresh := false }
  let badSchemaOffer := { enrollmentOffer with schemaCompatible := false }
  let badServerTicket := { enrollmentOffer with serverTicketPeer := "foreign-peer" }
  let badServerDid := { enrollmentOffer with resolvedServerDid := "did:key:foreign" }
  let unsignedRequest := { enrollmentRequest with candidateSigned := false }
  let expiredRequest := { enrollmentRequest with fresh := false }
  let badObservedPeer := { enrollmentRequest with observedCandidatePeer := "foreign-peer" }
  let badCandidateDid := { enrollmentRequest with resolvedCandidateDid := "did:key:foreign" }
  let badCandidateTicket := { enrollmentRequest with candidateTicketPeer := "foreign-peer" }
  let badDigest := { enrollmentRequest with
    digest := { emptyDigest with renderedHexBytes := [0] } }
  let wrongNetworkUnsigned := { unsignedEnrollmentRequest with networkId := "network-foreign" }
  let wrongNetwork := canonicalizeEnrollmentRequest wrongNetworkUnsigned
  let collisionOffer := { enrollmentOffer with offerId := "offer-2", challenge := "challenge-2" }
  let collisionUnsigned := { unsignedEnrollmentRequest with
    offerId := collisionOffer.offerId, challenge := collisionOffer.challenge,
    clientNonce := "nonce-2" }
  let collisionRequest := canonicalizeEnrollmentRequest collisionUnsigned
  let challengeUnsigned := { unsignedEnrollmentRequest with
    requestId := "request-2", clientNonce := "nonce-2" }
  let challengeRequest := canonicalizeEnrollmentRequest challengeUnsigned
  let deniedDecision := decisionFor enrollmentRequest .denied
  let wrongAdminDecision := { enrollmentDecision with signerDid := "did:key:foreign" }
  let expiredDecision := { enrollmentDecision with fresh := false }
  let equalRevocation := revocationFor enrollmentRequest 1
  let lowerRevocation := revocationFor enrollmentRequest 0
  let wrongSignerRevocation := { enrollmentRevocation with signerDid := "did:key:foreign" }
  let unsignedRevocation := { enrollmentRevocation with adminSigned := false }
  let wrongBindingRevocation := { enrollmentRevocation with memberPeer := "foreign-peer" }
  let replacementOffer := { enrollmentOffer with
    offerId := "offer-2", challenge := "challenge-2", ownerAgent := "did:key:agent-2" }
  let replacementUnsigned := { unsignedEnrollmentRequest with
    requestId := "request-2", offerId := replacementOffer.offerId,
    challenge := replacementOffer.challenge, candidatePeer := "candidate-peer-2",
    observedCandidatePeer := "candidate-peer-2", candidateTicketPeer := "candidate-peer-2",
    ownerAgent := replacementOffer.ownerAgent, clientNonce := "nonce-2" }
  let replacementRequest := canonicalizeEnrollmentRequest replacementUnsigned
  let replacementDecision := decisionFor replacementRequest .approved 2
  let conflictingAdminOffer := { enrollmentOffer with
    offerId := "offer-conflict", challenge := "challenge-conflict",
    adminDid := "did:key:admin-conflict", resolvedServerDid := "did:key:admin-conflict" }
  let conflictingAdminUnsigned := { unsignedEnrollmentRequest with
    requestId := "request-conflict", offerId := conflictingAdminOffer.offerId,
    challenge := conflictingAdminOffer.challenge, adminDid := conflictingAdminOffer.adminDid,
    clientNonce := "nonce-conflict" }
  let conflictingAdminRequest := canonicalizeEnrollmentRequest conflictingAdminUnsigned
  let otherNetworkOffer := { enrollmentOffer with
    offerId := "offer-network-2", challenge := "challenge-network-2",
    networkId := "network-2", adminDid := "did:key:admin-2",
    resolvedServerDid := "did:key:admin-2" }
  let otherNetworkUnsigned := { unsignedEnrollmentRequest with
    requestId := "request-network-2", offerId := otherNetworkOffer.offerId,
    challenge := otherNetworkOffer.challenge, networkId := otherNetworkOffer.networkId,
    adminDid := otherNetworkOffer.adminDid, clientNonce := "nonce-network-2" }
  let otherNetworkRequest := canonicalizeEnrollmentRequest otherNetworkUnsigned
  let conflictingActive := { revisionForApproval enrollmentRequest enrollmentDecision with
    memberPeer := "hostile-peer", signerDid := "did:key:hostile-replica" }
  let conflictingRevoked := { revisionForApproval enrollmentRequest enrollmentDecision with
    kind := RevisionKind.revoked }
  let unsignedReceipt := { enrollmentRouteReceipt with adminSigned := false }
  let wrongGenerationReceipt := { enrollmentRouteReceipt with authorizationSequence := 2 }
  [ enrollmentCase "status_first_ordering"
      [.accept enrollmentOffer enrollmentRequest, .observe enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest]
  , enrollmentCase "pin_creation_then_bidirectional_hydration_ready" validEnrollmentActions
  , enrollmentCase "same_network_different_admin_pin_conflict"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .observe conflictingAdminOffer, .confirmPin conflictingAdminOffer,
       .accept conflictingAdminOffer conflictingAdminRequest]
  , enrollmentCase "different_network_admin_pin_independence"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .observe otherNetworkOffer, .confirmPin otherNetworkOffer,
       .accept otherNetworkOffer otherNetworkRequest]
  , invalidOfferCase "unsigned_offer_grants_nothing" unsignedOffer
  , invalidOfferCase "expired_offer_grants_nothing" expiredOffer
  , invalidOfferCase "schema_mismatch_grants_nothing" badSchemaOffer
  , invalidOfferCase "server_ticket_peer_mismatch_grants_nothing" badServerTicket
  , invalidOfferCase "server_transport_did_mismatch" badServerDid
  , invalidRequestCase "unsigned_candidate_grants_nothing" unsignedRequest
  , invalidRequestCase "expired_request_grants_nothing" expiredRequest
  , invalidRequestCase "observed_candidate_peer_mismatch" badObservedPeer
  , invalidRequestCase "candidate_transport_did_mismatch" badCandidateDid
  , invalidRequestCase "candidate_ticket_peer_mismatch" badCandidateTicket
  , invalidRequestCase "invalid_canonical_digest" badDigest
  , invalidRequestCase "request_offer_network_mismatch" wrongNetwork
  , enrollmentCase "exact_request_replay_idempotent"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .accept enrollmentOffer enrollmentRequest]
  , enrollmentCase "request_id_collision_rejected_at_binding"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .observe collisionOffer, .confirmPin collisionOffer,
       .accept collisionOffer collisionRequest]
  , enrollmentCase "challenge_replay_rejected_at_binding"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .accept enrollmentOffer challengeRequest]
  , enrollmentCase "denial_is_terminal"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .decide enrollmentRequest deniedDecision,
       .decide enrollmentRequest enrollmentDecision]
  , enrollmentCase "wrong_admin_signer_cannot_decide"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .decide enrollmentRequest wrongAdminDecision]
  , enrollmentCase "expired_decision_cannot_decide"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .decide enrollmentRequest expiredDecision]
  , enrollmentCase "approval_without_membership_not_ready"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .decide enrollmentRequest enrollmentDecision]
  , enrollmentCase "membership_without_routes_not_ready"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .decide enrollmentRequest enrollmentDecision,
       .membership enrollmentRequest enrollmentDecision]
  , enrollmentCase "local_route_without_server_receipt_not_ready"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .decide enrollmentRequest enrollmentDecision,
       .membership enrollmentRequest enrollmentDecision,
       .clientRoute enrollmentRequest enrollmentDecision]
  , enrollmentCase "unsigned_server_route_receipt_rejected"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .decide enrollmentRequest enrollmentDecision,
       .membership enrollmentRequest enrollmentDecision,
       .clientRoute enrollmentRequest enrollmentDecision,
       .serverReceipt enrollmentRequest enrollmentDecision unsignedReceipt]
  , enrollmentCase "wrong_generation_server_route_receipt_rejected"
      [.observe enrollmentOffer, .confirmPin enrollmentOffer,
       .accept enrollmentOffer enrollmentRequest,
       .decide enrollmentRequest enrollmentDecision,
       .membership enrollmentRequest enrollmentDecision,
       .clientRoute enrollmentRequest enrollmentDecision,
       .serverReceipt enrollmentRequest enrollmentDecision wrongGenerationReceipt]
  , enrollmentCase "revocation_dominates_stale_materialization"
      (validEnrollmentActions ++
        [.revoke enrollmentRequest enrollmentRevocation,
         .membership enrollmentRequest enrollmentDecision,
         .clientRoute enrollmentRequest enrollmentDecision,
         .serverReceipt enrollmentRequest enrollmentDecision enrollmentRouteReceipt])
  , enrollmentCase "equal_revocation_rejected"
      (validEnrollmentActions ++ [.revoke enrollmentRequest equalRevocation])
  , enrollmentCase "lower_revocation_rejected"
      (validEnrollmentActions ++ [.revoke enrollmentRequest lowerRevocation])
  , enrollmentCase "wrong_revocation_signer_rejected"
      (validEnrollmentActions ++ [.revoke enrollmentRequest wrongSignerRevocation])
  , enrollmentCase "unsigned_revocation_rejected"
      (validEnrollmentActions ++ [.revoke enrollmentRequest unsignedRevocation])
  , enrollmentCase "wrong_revocation_binding_rejected"
      (validEnrollmentActions ++ [.revoke enrollmentRequest wrongBindingRevocation])
  , enrollmentCase "higher_approval_replaces_peer_and_agent"
      (validEnrollmentActions ++
        [.observe replacementOffer, .confirmPin replacementOffer,
         .accept replacementOffer replacementRequest,
         .decide replacementRequest replacementDecision,
         .membership replacementRequest replacementDecision,
         .clientRoute replacementRequest replacementDecision,
         .serverReceipt replacementRequest replacementDecision
           (serverRouteReceiptFor replacementRequest replacementDecision)])
  , enrollmentCase "restored_equal_sequence_active_active_fails_closed"
      (validEnrollmentActions ++
        [.merge enrollmentRequest enrollmentDecision conflictingActive])
  , enrollmentCase "restored_equal_sequence_active_revoked_fails_closed"
      (validEnrollmentActions ++
        [.merge enrollmentRequest enrollmentDecision conflictingRevoked])
  ]

private def durableDocumentStep (action : EnrollmentAction) : EnrollmentTraceStep :=
  enrollmentTraceStep action {}

private def durableCase
    (name : String) (documents : List EnrollmentAction)
    (docs : DurableDocuments) (offer : Offer) (request : Request) (decision : Decision)
    (receipt : RouteReceipt) :
    EnrollmentDurableProjectionCase :=
  { name
  , documents := documents.map durableDocumentStep
  , expectedCurrentApproval := decide (durableCurrentApproval docs offer request decision)
  , expectedCurrentRouteReceipt := decide
      (durableCurrentServerRouteReceipt docs offer request decision receipt) }

def enrollmentDurableProjectionCases : List EnrollmentDurableProjectionCase :=
  let pin := adminPinFor enrollmentOffer
  let revision := revisionForApproval enrollmentRequest enrollmentDecision
  let baseDocs : DurableDocuments :=
    { offers := {enrollmentOffer}, adminPins := {pin}, requests := {enrollmentRequest}
    , decisions := {enrollmentDecision}, revisions := {revision}
    , routeReceipts := {enrollmentRouteReceipt} }
  let baseActions : List EnrollmentAction :=
    [.observe enrollmentOffer, .confirmPin enrollmentOffer,
     .accept enrollmentOffer enrollmentRequest,
     .decide enrollmentRequest enrollmentDecision,
     .merge enrollmentRequest enrollmentDecision revision,
     .serverReceipt enrollmentRequest enrollmentDecision enrollmentRouteReceipt]
  let collisionOffer := { enrollmentOffer with offerId := "offer-2", challenge := "challenge-2" }
  let collisionUnsigned := { unsignedEnrollmentRequest with
    offerId := collisionOffer.offerId, challenge := collisionOffer.challenge,
    clientNonce := "nonce-2" }
  let collisionRequest := canonicalizeEnrollmentRequest collisionUnsigned
  let challengeUnsigned := { unsignedEnrollmentRequest with
    requestId := "request-2", clientNonce := "nonce-2" }
  let challengeRequest := canonicalizeEnrollmentRequest challengeUnsigned
  let deniedDecision := decisionFor enrollmentRequest .denied
  let conflictingRevision := { revision with signerDid := "did:key:hostile-replica" }
  let conflictingReceipt := { enrollmentRouteReceipt with signerDid := "did:key:hostile-replica" }
  let requestIdConflictDocs := { baseDocs with
    offers := insert collisionOffer baseDocs.offers
    requests := insert collisionRequest baseDocs.requests }
  let challengeConflictDocs := { baseDocs with
    requests := insert challengeRequest baseDocs.requests }
  let decisionConflictDocs := { baseDocs with
    decisions := insert deniedDecision baseDocs.decisions }
  let revisionConflictDocs := { baseDocs with
    revisions := insert conflictingRevision baseDocs.revisions }
  let receiptConflictDocs := { baseDocs with
    routeReceipts := insert conflictingReceipt baseDocs.routeReceipts }
  [ durableCase "durable_unique_approval" baseActions baseDocs
      enrollmentOffer enrollmentRequest enrollmentDecision enrollmentRouteReceipt
  , durableCase "durable_request_id_conflict_fails_closed"
      (baseActions ++ [.observe collisionOffer, .accept collisionOffer collisionRequest])
      requestIdConflictDocs
      enrollmentOffer enrollmentRequest enrollmentDecision enrollmentRouteReceipt
  , durableCase "durable_challenge_conflict_fails_closed"
      (baseActions ++ [.accept enrollmentOffer challengeRequest])
      challengeConflictDocs
      enrollmentOffer enrollmentRequest enrollmentDecision enrollmentRouteReceipt
  , durableCase "durable_terminal_decision_conflict_fails_closed"
      (baseActions ++ [.decide enrollmentRequest deniedDecision])
      decisionConflictDocs
      enrollmentOffer enrollmentRequest enrollmentDecision enrollmentRouteReceipt
  , durableCase "durable_equal_revision_conflict_fails_closed"
      (baseActions ++ [.merge enrollmentRequest enrollmentDecision conflictingRevision])
      revisionConflictDocs
      enrollmentOffer enrollmentRequest enrollmentDecision enrollmentRouteReceipt
  , durableCase "durable_route_receipt_conflict_fails_closed"
      (baseActions ++
        [.serverReceipt enrollmentRequest enrollmentDecision conflictingReceipt])
      receiptConflictDocs
      enrollmentOffer enrollmentRequest enrollmentDecision enrollmentRouteReceipt
  ]

def enrollmentEncodingCases : List EnrollmentEncodingCase :=
  let renderedFrame (value : String) := utf8HexString (frameWireField (stringBytes value))
  [ { name := "empty", value := "", expectedFrame := "ff",
      actualFrame := renderedFrame "", frameMatches := decide (renderedFrame "" = "ff") }
  , { name := "non_ascii_utf8_bytes", value := "é", expectedFrame := "0000ffc3a9",
      actualFrame := renderedFrame "é",
      frameMatches := decide (renderedFrame "é" = "0000ffc3a9") }
  , { name := "embedded_colon", value := "a:b", expectedFrame := "000000ff613a62",
      actualFrame := renderedFrame "a:b",
      frameMatches := decide (renderedFrame "a:b" = "000000ff613a62") }
  , { name := "two_digit_boundary", value := "0123456789",
      expectedFrame := "00000000000000000000ff30313233343536373839",
      actualFrame := renderedFrame "0123456789",
      frameMatches := decide
        (renderedFrame "0123456789" = "00000000000000000000ff30313233343536373839") }
  ]

private def baseExpectedPayload : String :=
  "0000000000000000000000000000ff000000000000000000000000000000000000000000000000000000ff67656e74732d656e726f6c6c6d656e742d726571756573742d7631000000000000000000ff726571756573742d3100000000000000ff6f666665722d310000000000000000000000ff6368616c6c656e67652d31000000000000000000ff6e6574776f726b2d3100000000000000000000000000ff6469643a6b65793a61646d696e0000000000000000000000ff7365727665722d706565720000000000000000000000000000000000ff6469643a6b65793a63616e6469646174650000000000000000000000000000ff63616e6469646174652d7065657200000000000000000000000000ff6469643a6b65793a6167656e74000000000000ff636c69656e7400000000000000ff6e6f6e63652d3100ff3100ff32"

private def baseExpectedDigest : String :=
  "utf8hex-v1:" ++ baseExpectedPayload

private def edgeTextFields : List String := ["", "é", "a:b", "0123456789"]
private def edgeExpectedPayload : String :=
  "0000000000ff000000000000000000000000000000000000000000000000000000ff" ++
  "67656e74732d656e726f6c6c6d656e742d726571756573742d7631" ++
  "ff0000ffc3a9000000ff613a6200000000000000000000ff30313233343536373839"
private def edgeExpectedDigest : String :=
  "utf8hex-v1:" ++ edgeExpectedPayload

def enrollmentDigestCases : List EnrollmentDigestCase :=
  let baseFields := canonicalRequestTextFields enrollmentRequest
  let baseByteFields := textFieldsToBytes baseFields
  let basePayload := utf8HexString (canonicalSerializedFields baseByteFields)
  let baseDigest := renderDigestString (canonicalDigestFromFields baseByteFields)
  let edgeByteFields := textFieldsToBytes edgeTextFields
  let edgePayload := utf8HexString (canonicalSerializedFields edgeByteFields)
  let edgeDigest := renderDigestString (canonicalDigestFromFields edgeByteFields)
  [ { name := "full_request_v1", fields := baseFields
    , expectedPayload := baseExpectedPayload, actualPayload := basePayload
    , expectedDigest := baseExpectedDigest, actualDigest := baseDigest
    , payloadMatches := decide (basePayload = baseExpectedPayload)
    , digestMatches := decide (baseDigest = baseExpectedDigest) }
  , { name := "utf8_empty_colon_boundary", fields := edgeTextFields
    , expectedPayload := edgeExpectedPayload, actualPayload := edgePayload
    , expectedDigest := edgeExpectedDigest, actualDigest := edgeDigest
    , payloadMatches := decide (edgePayload = edgeExpectedPayload)
    , digestMatches := decide (edgeDigest = edgeExpectedDigest) }
  ]

end Conformance.ContractCases
