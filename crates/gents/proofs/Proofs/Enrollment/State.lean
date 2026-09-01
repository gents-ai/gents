import Proofs.Basic
import Proofs.SessionHydration.State
import Mathlib.Data.Finset.Basic

/-!
# Authenticated status-first enrollment

Status is observation only. A request is accepted only after its signed offer was
observed and the candidate signature, authenticated transport peer, resolved DID,
ticket, schema, scoped network trust, and canonical digest agree. Membership
authority is an ordered admin-signed revision. The maximal revision is the sole
owner of operational membership and routes; revocation is an authenticated
tombstone, never deletion of authorization history.
-/

namespace Enrollment

abbrev Did := String
abbrev PeerId := String
abbrev RequestId := String

abbrev WireBytes := List UInt8
abbrev CanonicalFields := List WireBytes

/-- A digest contains only actual serialized bytes and their rendered ASCII hex bytes. -/
structure Digest where
  serializedBytes : WireBytes
  renderedHexBytes : WireBytes
  deriving DecidableEq, Repr

def emptyDigest : Digest := { serializedBytes := [], renderedHexBytes := [] }

inductive DecisionKind where | approved | denied deriving DecidableEq, Repr
inductive RevisionKind where | active | revoked deriving DecidableEq, Repr
inductive RouteDirection where | clientToServer | serverToClient deriving DecidableEq, Repr

structure Offer where
  offerId : String
  challenge : String
  networkId : String
  adminDid : Did
  serverPeer : PeerId
  serverTicketPeer : PeerId
  resolvedServerDid : Did
  ownerAgent : Did
  profile : String
  schemaCompatible : Bool
  adminSigned : Bool
  fresh : Bool
  deriving DecidableEq, Repr

/-- Durable trust is owned by state and scoped to the exact network/admin pair. -/
structure NetworkAdminPin where
  networkId : String
  adminDid : Did
  deriving DecidableEq, Repr

structure Request where
  requestId : RequestId
  digest : Digest
  offerId : String
  challenge : String
  networkId : String
  adminDid : Did
  serverPeer : PeerId
  candidateDid : Did
  candidatePeer : PeerId
  observedCandidatePeer : PeerId
  resolvedCandidateDid : Did
  candidateTicketPeer : PeerId
  ownerAgent : Did
  profile : String
  clientNonce : String
  issuedAt : String
  expiresAt : String
  candidateSigned : Bool
  fresh : Bool
  deriving DecidableEq, Repr

def enrollmentDigestDomain : String := "gents-enrollment-request-v1"

def stringBytes (value : String) : WireBytes := value.toUTF8.data.toList

def canonicalRequestTextFields (r : Request) : List String :=
  [r.requestId, r.offerId, r.challenge, r.networkId, r.adminDid,
   r.serverPeer, r.candidateDid, r.candidatePeer, r.ownerAgent, r.profile,
   r.clientNonce, r.issuedAt, r.expiresAt]

def textFieldsToBytes (fields : List String) : CanonicalFields := fields.map stringBytes

/-- The exact canonical signed-field boundary is UTF-8 bytes, not host strings. -/
def canonicalRequestFields (r : Request) : CanonicalFields :=
  textFieldsToBytes (canonicalRequestTextFields r)

/-- Self-delimiting unary natural: `n` zero bytes followed by `0xff`. -/
def encodeWireLength : Nat → WireBytes
  | 0 => [255]
  | n + 1 => 0 :: encodeWireLength n

/-- A field is its byte length followed by exactly those bytes. -/
def frameWireField (field : WireBytes) : WireBytes :=
  encodeWireLength field.length ++ field

def serializeWireFieldList : CanonicalFields → WireBytes
  | [] => []
  | field :: fields => frameWireField field ++ serializeWireFieldList fields

/-- The list count makes the complete field-list serialization self-delimiting. -/
def serializeWireFields (fields : CanonicalFields) : WireBytes :=
  encodeWireLength fields.length ++ serializeWireFieldList fields

def readWireLength : WireBytes → Option (Nat × WireBytes)
  | [] => none
  | byte :: bytes =>
      if byte = 255 then some (0, bytes)
      else if byte = 0 then
        match readWireLength bytes with
        | some (length, rest) => some (length + 1, rest)
        | none => none
      else none

def decodeWireFields : Nat → WireBytes → Option (CanonicalFields × WireBytes)
  | 0, bytes => some ([], bytes)
  | count + 1, bytes =>
      match readWireLength bytes with
      | none => none
      | some (length, rest) =>
          if length ≤ rest.length then
            match decodeWireFields count (rest.drop length) with
            | some (fields, suffix) => some (rest.take length :: fields, suffix)
            | none => none
          else none

def deserializeWireFields (bytes : WireBytes) : Option CanonicalFields :=
  match readWireLength bytes with
  | none => none
  | some (count, rest) =>
      match decodeWireFields count rest with
      | some (fields, []) => some fields
      | _ => none

def canonicalSerializedFields (fields : CanonicalFields) : WireBytes :=
  serializeWireFields (stringBytes enrollmentDigestDomain :: fields)

def canonicalRequestPayload (r : Request) : WireBytes :=
  canonicalSerializedFields (canonicalRequestFields r)

private def hexDigit : Nat → String
  | 0 => "0" | 1 => "1" | 2 => "2" | 3 => "3"
  | 4 => "4" | 5 => "5" | 6 => "6" | 7 => "7"
  | 8 => "8" | 9 => "9" | 10 => "a" | 11 => "b"
  | 12 => "c" | 13 => "d" | 14 => "e" | _ => "f"

private def hexByteString (byte : UInt8) : String :=
  hexDigit (byte.toNat / 16) ++ hexDigit (byte.toNat % 16)

def hexAsciiNibble (n : Nat) : UInt8 :=
  (if n < 10 then 48 + n else 87 + n).toUInt8

def hexHighByte (byte : UInt8) : UInt8 := hexAsciiNibble (byte.toNat / 16)
def hexLowByte (byte : UInt8) : UInt8 := hexAsciiNibble (byte.toNat % 16)

/-- Lower-case hexadecimal rendered as its actual ASCII/UTF-8 bytes. -/
def utf8HexBytes : WireBytes → WireBytes
  | [] => []
  | byte :: bytes => hexHighByte byte :: hexLowByte byte :: utf8HexBytes bytes

def utf8HexString (bytes : WireBytes) : String :=
  String.join <| bytes.map hexByteString

/-!
The proof model uses a reversible byte-exact digest representation rather than
duplicating a cryptographic implementation. Production may hash these exact
payload bytes, while conformance can compare the payload and this collision-free
model digest without relying on a runtime-specific hash primitive.
-/
def canonicalDigestFromFields (fields : CanonicalFields) : Digest :=
  let serialized := canonicalSerializedFields fields
  { serializedBytes := serialized, renderedHexBytes := utf8HexBytes serialized }

def digestRenderPrefix : WireBytes := stringBytes "utf8hex-v1:"

def renderDigest (digest : Digest) : WireBytes :=
  digestRenderPrefix ++ digest.renderedHexBytes

def renderDigestString (digest : Digest) : String :=
  "utf8hex-v1:" ++ utf8HexString digest.serializedBytes

/-- External obligation for a production hash over the exact serialized wire bytes. -/
def ProductionHashCollisionResistant (hash : WireBytes → String) : Prop :=
  Function.Injective hash

def canonicalRequestDigest (r : Request) : Digest :=
  canonicalDigestFromFields (canonicalRequestFields r)

structure ChallengeBinding where
  challenge : String
  requestId : RequestId
  digest : Digest
  deriving DecidableEq, Repr

structure RequestBinding where
  requestId : RequestId
  digest : Digest
  deriving DecidableEq, Repr

structure Decision where
  requestId : RequestId
  requestDigest : Digest
  networkId : String
  adminDid : Did
  candidateDid : Did
  candidatePeer : PeerId
  ownerAgent : Did
  kind : DecisionKind
  authorizationSequence : Nat
  /-- Signed lease boundary. `fresh` is the verifier's result at observation time. -/
  authorizationExpiresAt : String
  signerDid : Did
  adminSigned : Bool
  fresh : Bool
  deriving DecidableEq, Repr

structure AuthorizationRevision where
  requestId : RequestId
  requestDigest : Digest
  networkId : String
  adminDid : Did
  memberDid : Did
  memberPeer : PeerId
  ownerAgent : Did
  sequence : Nat
  /-- Exact lease generation copied from the approval; revocations retain it as history. -/
  authorizationExpiresAt : String
  kind : RevisionKind
  signerDid : Did
  adminSigned : Bool
  deriving DecidableEq, Repr

structure Membership where
  requestId : RequestId
  requestDigest : Digest
  networkId : String
  memberDid : Did
  memberPeer : PeerId
  ownerAgent : Did
  authorizationSequence : Nat
  authorizationExpiresAt : String
  active : Bool
  adminSigned : Bool
  fresh : Bool
  deriving DecidableEq, Repr

structure AppliedRoute where
  requestId : RequestId
  networkId : String
  authorizationSequence : Nat
  authorizationExpiresAt : String
  direction : RouteDirection
  peer : PeerId
  requester : Did
  agent : Did
  profile : String
  live : Bool
  deriving DecidableEq, Repr

/--
Admin-signed evidence that the runtime reconciled the exact client-to-server
route for one authorization generation.  The receipt is evidence, not
authority: a later authorization revision makes it non-current.
-/
structure RouteReceipt where
  requestId : RequestId
  requestDigest : Digest
  networkId : String
  adminDid : Did
  memberDid : Did
  memberPeer : PeerId
  serverPeer : PeerId
  ownerAgent : Did
  authorizationSequence : Nat
  authorizationExpiresAt : String
  direction : RouteDirection
  signerDid : Did
  adminSigned : Bool
  applied : Bool
  deriving DecidableEq, Repr

structure State where
  observedOffers : Finset Offer := ∅
  adminPins : Finset NetworkAdminPin := ∅
  acceptedRequests : Finset Request := ∅
  challengeBindings : Finset ChallengeBinding := ∅
  requestBindings : Finset RequestBinding := ∅
  decisions : Finset Decision := ∅
  authorizations : Finset AuthorizationRevision := ∅
  memberships : Finset Membership := ∅
  routeReceipts : Finset RouteReceipt := ∅
  appliedRoutes : Finset AppliedRoute := ∅
  deriving DecidableEq

def challengeBindingFor (r : Request) : ChallengeBinding :=
  { challenge := r.challenge, requestId := r.requestId, digest := r.digest }

def requestBindingFor (r : Request) : RequestBinding :=
  { requestId := r.requestId, digest := r.digest }

def adminPinFor (o : Offer) : NetworkAdminPin :=
  { networkId := o.networkId, adminDid := o.adminDid }

def adminPinConflict (s : State) (o : Offer) : Prop :=
  ∃ pin ∈ s.adminPins, pin.networkId = o.networkId ∧ pin.adminDid ≠ o.adminDid

instance (s : State) (o : Offer) : Decidable (adminPinConflict s o) := by
  unfold adminPinConflict; infer_instance

/-- First use is explicit and may only pin an observed, authenticated status offer. -/
def adminPinAdmissible (s : State) (o : Offer) : Prop :=
  o ∈ s.observedOffers ∧ o.adminSigned = true ∧ o.fresh = true ∧
  o.serverTicketPeer = o.serverPeer ∧ o.resolvedServerDid = o.adminDid ∧
  ¬ adminPinConflict s o

instance (s : State) (o : Offer) : Decidable (adminPinAdmissible s o) := by
  unfold adminPinAdmissible; infer_instance

/-- Trust is exact and unique; even a restored conflicting pin fails closed. -/
def networkAdminPinned (s : State) (o : Offer) : Prop :=
  adminPinFor o ∈ s.adminPins ∧ ¬ adminPinConflict s o

instance (s : State) (o : Offer) : Decidable (networkAdminPinned s o) := by
  unfold networkAdminPinned; infer_instance

def networkAdminPairPinned (s : State) (networkId : String) (adminDid : Did) : Prop :=
  { networkId, adminDid } ∈ s.adminPins ∧
  ∀ pin ∈ s.adminPins, pin.networkId = networkId → pin.adminDid = adminDid

instance (s : State) (networkId : String) (adminDid : Did) : Decidable
    (networkAdminPairPinned s networkId adminDid) := by
  unfold networkAdminPairPinned; infer_instance

def requestMatchesOffer (o : Offer) (r : Request) : Prop :=
  r.offerId = o.offerId ∧ r.challenge = o.challenge ∧
  r.networkId = o.networkId ∧ r.adminDid = o.adminDid ∧
  r.serverPeer = o.serverPeer ∧ r.ownerAgent = o.ownerAgent ∧
  r.profile = o.profile

instance (o : Offer) (r : Request) : Decidable (requestMatchesOffer o r) := by
  unfold requestMatchesOffer; infer_instance

def challengeBoundElsewhere (s : State) (r : Request) : Prop :=
  ∃ binding ∈ s.challengeBindings,
    binding.challenge = r.challenge ∧ binding ≠ challengeBindingFor r

def requestIdBoundElsewhere (s : State) (r : Request) : Prop :=
  ∃ binding ∈ s.requestBindings,
    binding.requestId = r.requestId ∧ binding ≠ requestBindingFor r

instance (s : State) (r : Request) : Decidable (challengeBoundElsewhere s r) := by
  unfold challengeBoundElsewhere; infer_instance
instance (s : State) (r : Request) : Decidable (requestIdBoundElsewhere s r) := by
  unfold requestIdBoundElsewhere; infer_instance

def requestAdmissible (s : State) (o : Offer) (r : Request) : Prop :=
  o ∈ s.observedOffers ∧
  networkAdminPinned s o ∧
  o.adminSigned = true ∧ o.fresh = true ∧
  o.schemaCompatible = true ∧ o.serverTicketPeer = o.serverPeer ∧
  o.resolvedServerDid = o.adminDid ∧
  r.candidateSigned = true ∧ r.fresh = true ∧
  r.digest = canonicalRequestDigest r ∧
  r.observedCandidatePeer = r.candidatePeer ∧
  r.resolvedCandidateDid = r.candidateDid ∧
  r.candidateTicketPeer = r.candidatePeer ∧
  o.profile = "client" ∧ requestMatchesOffer o r ∧
  ¬ challengeBoundElsewhere s r ∧ ¬ requestIdBoundElsewhere s r

instance (s : State) (o : Offer) (r : Request) : Decidable (requestAdmissible s o r) := by
  unfold requestAdmissible; infer_instance

def terminalDecisionFor (s : State) (requestId : RequestId) : Prop :=
  ∃ decision ∈ s.decisions, decision.requestId = requestId

instance (s : State) (requestId : RequestId) : Decidable (terminalDecisionFor s requestId) := by
  unfold terminalDecisionFor; infer_instance

def decisionMatchesRequest (r : Request) (d : Decision) : Prop :=
  d.requestId = r.requestId ∧ d.requestDigest = r.digest ∧
  d.networkId = r.networkId ∧ d.adminDid = r.adminDid ∧
  d.candidateDid = r.candidateDid ∧ d.candidatePeer = r.candidatePeer ∧
  d.ownerAgent = r.ownerAgent ∧ d.signerDid = r.adminDid ∧
  d.authorizationExpiresAt ≠ ""

instance (r : Request) (d : Decision) : Decidable (decisionMatchesRequest r d) := by
  unfold decisionMatchesRequest; infer_instance

def revisionForApproval (r : Request) (d : Decision) : AuthorizationRevision :=
  { requestId := r.requestId, requestDigest := r.digest
  , networkId := r.networkId, adminDid := r.adminDid
  , memberDid := r.candidateDid, memberPeer := r.candidatePeer
  , ownerAgent := r.ownerAgent, sequence := d.authorizationSequence
  , authorizationExpiresAt := d.authorizationExpiresAt
  , kind := .active, signerDid := d.signerDid, adminSigned := d.adminSigned }

def revisionMatchesRequest (r : Request) (revision : AuthorizationRevision) : Prop :=
  revision.requestId = r.requestId ∧ revision.requestDigest = r.digest ∧
  revision.networkId = r.networkId ∧ revision.adminDid = r.adminDid ∧
  revision.memberDid = r.candidateDid ∧ revision.memberPeer = r.candidatePeer ∧
  revision.ownerAgent = r.ownerAgent ∧ revision.signerDid = r.adminDid ∧
  revision.authorizationExpiresAt ≠ ""

instance (r : Request) (revision : AuthorizationRevision) : Decidable
    (revisionMatchesRequest r revision) := by
  unfold revisionMatchesRequest; infer_instance

def sameMember (a b : AuthorizationRevision) : Prop :=
  a.networkId = b.networkId ∧ a.memberDid = b.memberDid

instance (a b : AuthorizationRevision) : Decidable (sameMember a b) := by
  unfold sameMember; infer_instance

def dominatingRevisionExists (s : State) (revision : AuthorizationRevision) : Prop :=
  ∃ current ∈ s.authorizations,
    sameMember current revision ∧ revision.sequence ≤ current.sequence

instance (s : State) (r : AuthorizationRevision) : Decidable (dominatingRevisionExists s r) := by
  unfold dominatingRevisionExists; infer_instance

/--
An authorization is current only when it is the unique maximum for the scoped
member. Equal-sequence conflicting restored or replicated revisions therefore
make every contender non-current, even though serial insertion rejects ties.
-/
def uniqueMaximumRevision (s : State) (revision : AuthorizationRevision) : Prop :=
  revision ∈ s.authorizations ∧
  ∀ other ∈ s.authorizations, sameMember other revision →
    other.sequence < revision.sequence ∨ other = revision

instance (s : State) (revision : AuthorizationRevision) : Decidable
    (uniqueMaximumRevision s revision) := by
  unfold uniqueMaximumRevision; infer_instance

/-- An arbitrary restored/replicated authorization merge; projection remains fail closed. -/
def mergeAuthorization (s : State) (revision : AuthorizationRevision) : State :=
  { s with authorizations := insert revision s.authorizations }

/-- Serial transitions preserve this; projection safety does not assume it after restore/merge. -/
def AuthorizationWellFormed (s : State) : Prop :=
  ∀ a ∈ s.authorizations, ∀ b ∈ s.authorizations,
    sameMember a b → a.sequence = b.sequence → a = b

def decisionAdmissible (s : State) (r : Request) (d : Decision) : Prop :=
  r ∈ s.acceptedRequests ∧ decisionMatchesRequest r d ∧
  d.adminSigned = true ∧ d.fresh = true ∧
  networkAdminPairPinned s r.networkId r.adminDid ∧
  ¬ terminalDecisionFor s r.requestId ∧
  (d.kind = .denied ∨
    (d.kind = .approved ∧ ¬ dominatingRevisionExists s (revisionForApproval r d)))

instance (s : State) (r : Request) (d : Decision) : Decidable
    (decisionAdmissible s r d) := by
  unfold decisionAdmissible; infer_instance

def currentApproval (s : State) (r : Request) (d : Decision) : Prop :=
  r ∈ s.acceptedRequests ∧ d ∈ s.decisions ∧ d.kind = .approved ∧ decisionMatchesRequest r d ∧
  d.adminSigned = true ∧ d.fresh = true ∧
  networkAdminPairPinned s r.networkId r.adminDid ∧
  uniqueMaximumRevision s (revisionForApproval r d)

instance (s : State) (r : Request) (d : Decision) : Decidable (currentApproval s r d) := by
  unfold currentApproval; infer_instance

/-- Operational peer admission comes only from an exact current enrollment. -/
def peerOperationallyAuthorized (s : State) (memberDid : Did) : Prop :=
  ∃ r ∈ s.acceptedRequests, ∃ d ∈ s.decisions,
    currentApproval s r d ∧ r.candidateDid = memberDid

instance (s : State) (memberDid : Did) : Decidable
    (peerOperationallyAuthorized s memberDid) := by
  unfold peerOperationallyAuthorized; infer_instance

/--
The operational admission projector accepts legacy materialization rows only as
an explicit ignored input. They may witness effects but never grant authority.
-/
def projectsPeerAdmission (s : State) (_legacyDesiredPeers : Finset Did)
    (memberDid : Did) : Prop :=
  peerOperationallyAuthorized s memberDid

instance (s : State) (legacyDesiredPeers : Finset Did) (memberDid : Did) : Decidable
    (projectsPeerAdmission s legacyDesiredPeers memberDid) := by
  unfold projectsPeerAdmission; infer_instance

def membershipFor (r : Request) (d : Decision) : Membership :=
  { requestId := r.requestId, requestDigest := r.digest
  , networkId := r.networkId, memberDid := r.candidateDid
  , memberPeer := r.candidatePeer, ownerAgent := r.ownerAgent
  , authorizationSequence := d.authorizationSequence
  , authorizationExpiresAt := d.authorizationExpiresAt
  , active := true, adminSigned := true, fresh := true }

def clientToServerRoute (r : Request) (d : Decision) : AppliedRoute :=
  { requestId := r.requestId, networkId := r.networkId
  , authorizationSequence := d.authorizationSequence
  , authorizationExpiresAt := d.authorizationExpiresAt, direction := .clientToServer
  , peer := r.candidatePeer, requester := r.candidateDid
  , agent := r.ownerAgent, profile := r.profile, live := true }

def serverToClientRoute (r : Request) (d : Decision) : AppliedRoute :=
  { requestId := r.requestId, networkId := r.networkId
  , authorizationSequence := d.authorizationSequence
  , authorizationExpiresAt := d.authorizationExpiresAt, direction := .serverToClient
  , peer := r.serverPeer, requester := r.candidateDid
  , agent := r.ownerAgent, profile := r.profile, live := true }

def serverRouteReceiptFor (r : Request) (d : Decision) : RouteReceipt :=
  { requestId := r.requestId, requestDigest := r.digest
  , networkId := r.networkId, adminDid := r.adminDid
  , memberDid := r.candidateDid, memberPeer := r.candidatePeer
  , serverPeer := r.serverPeer, ownerAgent := r.ownerAgent
  , authorizationSequence := d.authorizationSequence
  , authorizationExpiresAt := d.authorizationExpiresAt
  , direction := .clientToServer, signerDid := r.adminDid
  , adminSigned := true, applied := true }

def routeReceiptMatchesApproval (r : Request) (d : Decision) (receipt : RouteReceipt) : Prop :=
  receipt = serverRouteReceiptFor r d

instance (r : Request) (d : Decision) (receipt : RouteReceipt) : Decidable
    (routeReceiptMatchesApproval r d receipt) := by
  unfold routeReceiptMatchesApproval; infer_instance

def currentServerRouteReceipt (s : State) (r : Request) (d : Decision)
    (receipt : RouteReceipt) : Prop :=
  currentApproval s r d ∧ receipt ∈ s.routeReceipts ∧
  routeReceiptMatchesApproval r d receipt ∧
  receipt.adminSigned = true ∧ receipt.applied = true

instance (s : State) (r : Request) (d : Decision) (receipt : RouteReceipt) : Decidable
    (currentServerRouteReceipt s r d receipt) := by
  unfold currentServerRouteReceipt; infer_instance

def membershipOwnedBy (r : Request) (membership : Membership) : Prop :=
  membership.networkId = r.networkId ∧ membership.memberDid = r.candidateDid

def routeOwnedBy (r : Request) (route : AppliedRoute) : Prop :=
  route.networkId = r.networkId ∧ route.requester = r.candidateDid

instance (r : Request) (m : Membership) : Decidable (membershipOwnedBy r m) := by
  unfold membershipOwnedBy; infer_instance
instance (r : Request) (route : AppliedRoute) : Decidable (routeOwnedBy r route) := by
  unfold routeOwnedBy; infer_instance

def membershipCurrentlyAuthorized (s : State) (membership : Membership) : Prop :=
  ∃ r ∈ s.acceptedRequests, ∃ d ∈ s.decisions,
    currentApproval s r d ∧ membership = membershipFor r d

def routeForDirection (r : Request) (d : Decision) : RouteDirection → AppliedRoute
  | .clientToServer => clientToServerRoute r d
  | .serverToClient => serverToClientRoute r d

def routeCurrentlyAuthorized (s : State) (direction : RouteDirection)
    (route : AppliedRoute) : Prop :=
  ∃ r ∈ s.acceptedRequests, ∃ d ∈ s.decisions,
    currentApproval s r d ∧ membershipFor r d ∈ s.memberships ∧
    route = routeForDirection r d direction

instance (s : State) (direction : RouteDirection) (route : AppliedRoute) : Decidable
    (routeCurrentlyAuthorized s direction route) := by
  unfold routeCurrentlyAuthorized; infer_instance

def clientRouteCurrentlyAuthorized (s : State) (route : AppliedRoute) : Prop :=
  routeCurrentlyAuthorized s .clientToServer route

def serverRouteCurrentlyAuthorized (s : State) (route : AppliedRoute) : Prop :=
  routeCurrentlyAuthorized s .serverToClient route

instance (s : State) (membership : Membership) : Decidable
    (membershipCurrentlyAuthorized s membership) := by
  unfold membershipCurrentlyAuthorized; infer_instance
instance (s : State) (route : AppliedRoute) : Decidable
    (clientRouteCurrentlyAuthorized s route) := by
  unfold clientRouteCurrentlyAuthorized; infer_instance
instance (s : State) (route : AppliedRoute) : Decidable
    (serverRouteCurrentlyAuthorized s route) := by
  unfold serverRouteCurrentlyAuthorized; infer_instance

def enrollmentReady (s : State) (r : Request) : Prop :=
  ∃ d ∈ s.decisions, currentApproval s r d ∧
    membershipFor r d ∈ s.memberships ∧
    serverRouteReceiptFor r d ∈ s.routeReceipts ∧
    clientToServerRoute r d ∈ s.appliedRoutes ∧
    serverToClientRoute r d ∈ s.appliedRoutes

instance (s : State) (r : Request) : Decidable (enrollmentReady s r) := by
  unfold enrollmentReady; infer_instance

def toHydrationRoute (route : AppliedRoute) : SessionHydration.AppliedPairingRoute :=
  { peer := route.peer, requester := route.requester, agent := route.agent }

def toHydrationMembership (membership : Membership) : SessionHydration.VerifiedActiveMembership :=
  { network := membership.networkId, member := membership.memberDid }

def projectedHydrationCatalogFor (s : State) (selectedNetwork : String)
    (direction : RouteDirection)
    (sessions : Finset SessionHydration.SessionOwner) : SessionHydration.Catalog :=
  { appliedPairingRoutes :=
      (s.appliedRoutes.filter fun (route : AppliedRoute) =>
        route.networkId = selectedNetwork ∧ route.direction = direction ∧
          route.live = true ∧ routeCurrentlyAuthorized s direction route).image toHydrationRoute
  , selectedNetwork
  , verifiedActiveMemberships :=
      (s.memberships.filter fun membership =>
        membership.networkId = selectedNetwork ∧ membership.active = true ∧
          membership.adminSigned = true ∧ membership.fresh = true ∧
          membershipCurrentlyAuthorized s membership).image toHydrationMembership
  , sessions
  , documents := ∅ }

def projectedClientToServerHydrationCatalog (s : State) (selectedNetwork : String)
    (sessions : Finset SessionHydration.SessionOwner) : SessionHydration.Catalog :=
  projectedHydrationCatalogFor s selectedNetwork .clientToServer sessions

def projectedServerToClientHydrationCatalog (s : State) (selectedNetwork : String)
    (sessions : Finset SessionHydration.SessionOwner) : SessionHydration.Catalog :=
  projectedHydrationCatalogFor s selectedNetwork .serverToClient sessions

def projectedHydrationCatalog := projectedClientToServerHydrationCatalog

def hydrationRequestForDirection (r : Request) (session : String) :
    RouteDirection → SessionHydration.Request
  | .clientToServer =>
    { key := r.requestId, peer := r.candidatePeer, requester := r.candidateDid
    , agent := r.ownerAgent, session }
  | .serverToClient =>
    { key := r.requestId, peer := r.serverPeer, requester := r.candidateDid
    , agent := r.ownerAgent, session }

def hydrationRequestFor (r : Request) (session : String) : SessionHydration.Request :=
  hydrationRequestForDirection r session .clientToServer

def reverseHydrationRequestFor (r : Request) (session : String) : SessionHydration.Request :=
  { key := r.requestId, peer := r.serverPeer, requester := r.candidateDid
  , agent := r.ownerAgent, session }

end Enrollment
