import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Family

namespace Conformance.EnrollmentCore

open Lean Enrollment Conformance.ContractCases

structure StepInput where
  action : String
  peerAdmissionDid : String
  offerId : String
  offerChallenge : String
  offerNetworkId : String
  offerAdminDid : String
  offerServerPeer : String
  offerOwnerNode : String
  offerProfile : String
  challenge : String
  requestId : String
  requestDigest : String
  requestOfferId : String
  networkId : String
  adminDid : String
  serverPeer : String
  serverTicketPeer : String
  resolvedServerDid : String
  profile : String
  schemaCompatible : Bool
  offerAdminSigned : Bool
  offerFresh : Bool
  candidateDid : String
  candidatePeer : String
  observedCandidatePeer : String
  resolvedCandidateDid : String
  candidateTicketPeer : String
  ownerNode : String
  clientNonce : String
  issuedAt : String
  expiresAt : String
  candidateSigned : Bool
  requestFresh : Bool
  decisionAuthorizationSequence : Nat
  decisionAuthorizationExpiresAt : String
  decisionSignerDid : String
  decisionKind : String
  decisionRequestId : String
  decisionRequestDigest : String
  decisionNetworkId : String
  decisionAdminDid : String
  decisionCandidateDid : String
  decisionCandidatePeer : String
  decisionOwnerNode : String
  decisionAdminSigned : Bool
  decisionFresh : Bool
  revisionKind : String
  revisionSequence : Nat
  revisionAuthorizationExpiresAt : String
  revisionSignerDid : String
  revisionRequestId : String
  revisionRequestDigest : String
  revisionNetworkId : String
  revisionAdminDid : String
  revisionMemberDid : String
  revisionMemberPeer : String
  revisionOwnerNode : String
  revisionAdminSigned : Bool
  receiptRequestId : String
  receiptRequestDigest : String
  receiptNetworkId : String
  receiptAdminDid : String
  receiptMemberDid : String
  receiptMemberPeer : String
  receiptServerPeer : String
  receiptOwnerNode : String
  receiptAuthorizationSequence : Nat
  receiptAuthorizationExpiresAt : String
  receiptDirection : String
  receiptSignerDid : String
  receiptAdminSigned : Bool
  receiptApplied : Bool
  deriving ToJson, BEq

def stepInput (s : EnrollmentTraceStep) : StepInput :=
  { action := s.action
    peerAdmissionDid := s.peerAdmissionDid
    offerId := s.offerId
    offerChallenge := s.offerChallenge
    offerNetworkId := s.offerNetworkId
    offerAdminDid := s.offerAdminDid
    offerServerPeer := s.offerServerPeer
    offerOwnerNode := s.offerOwnerNode
    offerProfile := s.offerProfile
    challenge := s.challenge
    requestId := s.requestId
    requestDigest := s.requestDigest
    requestOfferId := s.requestOfferId
    networkId := s.networkId
    adminDid := s.adminDid
    serverPeer := s.serverPeer
    serverTicketPeer := s.serverTicketPeer
    resolvedServerDid := s.resolvedServerDid
    profile := s.profile
    schemaCompatible := s.schemaCompatible
    offerAdminSigned := s.offerAdminSigned
    offerFresh := s.offerFresh
    candidateDid := s.candidateDid
    candidatePeer := s.candidatePeer
    observedCandidatePeer := s.observedCandidatePeer
    resolvedCandidateDid := s.resolvedCandidateDid
    candidateTicketPeer := s.candidateTicketPeer
    ownerNode := s.ownerNode
    clientNonce := s.clientNonce
    issuedAt := s.issuedAt
    expiresAt := s.expiresAt
    candidateSigned := s.candidateSigned
    requestFresh := s.requestFresh
    decisionAuthorizationSequence := s.decisionAuthorizationSequence
    decisionAuthorizationExpiresAt := s.decisionAuthorizationExpiresAt
    decisionSignerDid := s.decisionSignerDid
    decisionKind := s.decisionKind
    decisionRequestId := s.decisionRequestId
    decisionRequestDigest := s.decisionRequestDigest
    decisionNetworkId := s.decisionNetworkId
    decisionAdminDid := s.decisionAdminDid
    decisionCandidateDid := s.decisionCandidateDid
    decisionCandidatePeer := s.decisionCandidatePeer
    decisionOwnerNode := s.decisionOwnerNode
    decisionAdminSigned := s.decisionAdminSigned
    decisionFresh := s.decisionFresh
    revisionKind := s.revisionKind
    revisionSequence := s.revisionSequence
    revisionAuthorizationExpiresAt := s.revisionAuthorizationExpiresAt
    revisionSignerDid := s.revisionSignerDid
    revisionRequestId := s.revisionRequestId
    revisionRequestDigest := s.revisionRequestDigest
    revisionNetworkId := s.revisionNetworkId
    revisionAdminDid := s.revisionAdminDid
    revisionMemberDid := s.revisionMemberDid
    revisionMemberPeer := s.revisionMemberPeer
    revisionOwnerNode := s.revisionOwnerNode
    revisionAdminSigned := s.revisionAdminSigned
    receiptRequestId := s.receiptRequestId
    receiptRequestDigest := s.receiptRequestDigest
    receiptNetworkId := s.receiptNetworkId
    receiptAdminDid := s.receiptAdminDid
    receiptMemberDid := s.receiptMemberDid
    receiptMemberPeer := s.receiptMemberPeer
    receiptServerPeer := s.receiptServerPeer
    receiptOwnerNode := s.receiptOwnerNode
    receiptAuthorizationSequence := s.receiptAuthorizationSequence
    receiptAuthorizationExpiresAt := s.receiptAuthorizationExpiresAt
    receiptDirection := s.receiptDirection
    receiptSignerDid := s.receiptSignerDid
    receiptAdminSigned := s.receiptAdminSigned
    receiptApplied := s.receiptApplied }

structure StepObservation where
  observedOfferCount : Nat
  adminPinCount : Nat
  challengeBindingCount : Nat
  requestBindingCount : Nat
  requestCount : Nat
  decisionCount : Nat
  authorizationCount : Nat
  membershipCount : Nat
  receiptCount : Nat
  routeCount : Nat
  requestAccepted : Bool
  decisionRecorded : Bool
  authorizationRecorded : Bool
  revisionRecorded : Bool
  receiptRecorded : Bool
  membershipPresent : Bool
  clientRoutePresent : Bool
  serverRoutePresent : Bool
  adminPinPresent : Bool
  adminPinConflict : Bool
  challengeBindingConflict : Bool
  requestBindingConflict : Bool
  currentApproval : Bool
  peerAdmitted : Bool
  ready : Bool
  deriving ToJson, BEq

def stepObservation (s : EnrollmentTraceStep) : StepObservation :=
  { observedOfferCount := s.observedOfferCount
    adminPinCount := s.adminPinCount
    challengeBindingCount := s.challengeBindingCount
    requestBindingCount := s.requestBindingCount
    requestCount := s.requestCount
    decisionCount := s.decisionCount
    authorizationCount := s.authorizationCount
    membershipCount := s.membershipCount
    receiptCount := s.receiptCount
    routeCount := s.routeCount
    requestAccepted := s.requestAccepted
    decisionRecorded := s.decisionRecorded
    authorizationRecorded := s.authorizationRecorded
    revisionRecorded := s.revisionRecorded
    receiptRecorded := s.receiptRecorded
    membershipPresent := s.membershipPresent
    clientRoutePresent := s.clientRoutePresent
    serverRoutePresent := s.serverRoutePresent
    adminPinPresent := s.adminPinPresent
    adminPinConflict := s.adminPinConflict
    challengeBindingConflict := s.challengeBindingConflict
    requestBindingConflict := s.requestBindingConflict
    currentApproval := s.currentApproval
    peerAdmitted := s.peerAdmitted
    ready := s.ready }

structure TraceInput where
  name : String
  steps : List StepInput
  deriving ToJson

/-- Each case runs its named action list from the empty state; the steps are
that run's wire projection, so a case whose steps differ observes nothing. -/
@[conformance_family]
def trace : Family TraceInput (List StepObservation) where
  inputs := enrollmentCases.map fun c => { name := c.name, steps := c.steps.map stepInput }
  model := fun i => match enrollmentCases.find? (·.name == i.name) with
    | some c => if c.steps.map stepInput == i.steps then c.steps.map stepObservation else []
    | none => []

structure DurableInput where
  name : String
  documents : List StepInput
  deriving ToJson

structure DurableProjection where
  currentApproval : Bool
  currentRouteReceipt : Bool
  deriving ToJson

/-- Each case projects its named durable documents; the documents are their
wire projection, so a case whose documents differ projects nothing. -/
@[conformance_family]
def durableProjection : Family DurableInput (Option DurableProjection) where
  inputs := enrollmentDurableProjectionCases.map fun c =>
    { name := c.name, documents := c.documents.map stepInput }
  model := fun i => (enrollmentDurableProjectionCases.find? (·.name == i.name)).bind fun c =>
    if c.documents.map stepInput == i.documents then
      some { currentApproval := c.expectedCurrentApproval
             currentRouteReceipt := c.expectedCurrentRouteReceipt }
    else none

structure FrameInput where
  name : String
  value : String
  deriving ToJson

structure Frame where
  frame : String
  deriving ToJson

@[conformance_family]
def frame : Family FrameInput Frame where
  inputs := enrollmentEncodingCases.map fun c => { name := c.name, value := c.value }
  model := fun i => { frame := utf8HexString (frameWireField (stringBytes i.value)) }

example : enrollmentEncodingCases.all (·.frameMatches) = true := by native_decide

structure DigestInput where
  name : String
  fields : List String
  deriving ToJson

structure CanonicalDigest where
  payload : String
  digest : String
  deriving ToJson

@[conformance_family]
def digest : Family DigestInput CanonicalDigest where
  inputs := enrollmentDigestCases.map fun c => { name := c.name, fields := c.fields }
  model := fun i =>
    let fields := textFieldsToBytes i.fields
    { payload := utf8HexString (canonicalSerializedFields fields)
      digest := renderDigestString (canonicalDigestFromFields fields) }

example : enrollmentDigestCases.all (fun c => c.payloadMatches && c.digestMatches) = true := by
  native_decide

end Conformance.EnrollmentCore
