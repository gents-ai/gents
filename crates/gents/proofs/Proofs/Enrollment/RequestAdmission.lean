import Proofs.Enrollment.Transition
import Proofs.Enrollment.RequestInput
import Proofs.Request.Executable
import Proofs.Request.CausalHop

namespace Enrollment

inductive AgentRequestAdmissionKind where
  | enrollment
  /-- A client signs its own request; native document ACP is the authority for
  its database write. Its signing actor is distinct from immutable session
  requester provenance: a new session records the signer, while existing sessions
  retain their canonical requester even when another granted user acts. Enrollment admits P2P peers and is not a client document
  permission. The runtime never substitutes its Node or Agent signing key.
  Successful creation alone currently establishes ownership, not a target-agent
  invoke grant: full policy mapping remains unenforced pending #3485. -/
  | client
  | localSelf
  | runtimeInternal
  /-- A request authored by another node's agent: a cross-node
  `agent_new`/`agent_message`. The requester signs, the target differs
  from the requester, and the target's `PeerAdmissionAuthority` document ACP
  authorizes the requester DID. Premise: DefraDB authenticates the requester
  DID and enforces that ACP; the runtime adds no bridge row, claim fence,
  cancel mirror or delegated input. The reply routes back to `requesterDid`
  best-effort through the P2P templates. A same-node send is `localSelf`. -/
  | peer
  deriving DecidableEq, Repr

inductive RuntimeInternalSourceKind where
  | localControl
  | automatedTrigger
  deriving DecidableEq, Repr

/-- Parent-only provenance is signed by the title request, but is not a
delegation edge or authority to write under the parent request. -/
structure AuxiliaryParentLink where
  requestId : String
  documentId : String
  deriving DecidableEq, Repr

structure AuxiliaryParentEvidence where
  link : AuxiliaryParentLink
  nodeDid : Did
  sessionId : String
  agentDid : String
  logicalBindingCurrent : Bool
  physicalBindingCurrent : Bool
  deriving DecidableEq, Repr

/-- Native `push_option` uses byte tags 1/0; the native adapter
must map the typed link through its existing encoder, not interpret these model
labels as the native signature payload bytes. -/
def auxiliaryParentFields (link : AuxiliaryParentLink) : CanonicalFields :=
  textFieldsToBytes ["some", link.requestId, "some", link.documentId, "none", "none"]

structure AgentRequestSemantics where
  hop : Nat := 0
  requestId : String
  purpose : RequestPurpose
  targetNode : Did
  requesterDid : Did
  agentDid : String
  sessionId : String
  content : String
  input : RequestInput
  createdAt : String
  triggerConfigDocumentId : String
  retryFields : CanonicalFields
  triggerFields : CanonicalFields
  parentFields : CanonicalFields
  workspace : RequestWorkspace
  deriving DecidableEq, Repr

/-- These model bytes (one field whose length is the hop) are not the native
signature payload: native signing encodes the hop through its existing decimal
`request_hop` encoder, and the adapter maps the typed `hop` (emitted as a typed
JSON field) to that encoder. Both encodings are injective, so a changed hop
always changes the signed fields. -/
def hopField (hop : Nat) : WireBytes := List.replicate hop 0

def agentRequestSemanticFields (request : AgentRequestSemantics) : CanonicalFields :=
  [hopField request.hop] ++ textFieldsToBytes
    [ request.requestId, request.purpose.toWire, request.targetNode, request.requesterDid
    , request.agentDid, request.sessionId, request.content ] ++
  requestInputFields request.input ++ textFieldsToBytes
    [request.createdAt, request.triggerConfigDocumentId] ++
  request.retryFields ++ request.triggerFields ++ request.parentFields ++ requestWorkspaceFields request.workspace

structure AgentRequestAdmission where
  kind : AgentRequestAdmissionKind
  signerDid : Did
  enrollmentRequestId : RequestId
  enrollmentRequestDigest : Digest
  enrollmentAdminDid : Did
  enrollmentAuthorizationSequence : Nat
  enrollmentAuthorizationExpiresAt : String
  issuerDid : Did
  sourceRequestId : String
  runtimeSourceKind : RuntimeInternalSourceKind
  signedFields : CanonicalFields
  signatureValid : Bool
  deriving DecidableEq, Repr

def agentRequestAdmissionFields
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission) : CanonicalFields :=
  agentRequestSemanticFields request ++ textFieldsToBytes
    [ match admission.kind with
      | .enrollment => "enrollment"
      | .client => "client"
      | .localSelf => "local-self"
      | .runtimeInternal => "runtime-internal"
      | .peer => "peer"
    , admission.signerDid
    , admission.enrollmentRequestId
    , renderDigestString admission.enrollmentRequestDigest
    , admission.enrollmentAdminDid
    , toString admission.enrollmentAuthorizationSequence
    , admission.enrollmentAuthorizationExpiresAt
    , admission.issuerDid
    , admission.sourceRequestId
    , match admission.kind with
      | .runtimeInternal => match admission.runtimeSourceKind with
          | .localControl => "local-control"
          | .automatedTrigger => "automated-trigger"
      | _ => "" ]

theorem purpose_change_changes_signed_fields
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (hpurpose : request.purpose = .normal) :
    agentRequestAdmissionFields request admission ≠
      agentRequestAdmissionFields { request with purpose := .titleAudit } admission := by
  intro heq
  have hsecond := congrArg (fun fields => fields[2]?) heq
  simp [agentRequestAdmissionFields, agentRequestSemanticFields, textFieldsToBytes,
    hpurpose, RequestPurpose.toWire] at hsecond
  exact (by decide : stringBytes "normal" ≠ stringBytes "title-audit") hsecond

theorem hop_change_changes_signed_fields
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission) (hop : Nat)
    (hne : hop ≠ request.hop) :
    agentRequestAdmissionFields request admission ≠
      agentRequestAdmissionFields { request with hop := hop } admission := by
  intro heq
  have hfirst := congrArg List.head? heq
  simp only [agentRequestAdmissionFields, agentRequestSemanticFields, hopField,
    List.cons_append, List.singleton_append, List.head?_cons, Option.some.injEq] at hfirst
  have hlen := congrArg List.length hfirst
  simp only [List.length_replicate] at hlen
  exact hne hlen.symm

/-- Claim-time evidence reconstructed by the target runtime. These observations
are never trusted from request columns: the issuer signature, durable source
binding, and target policy are checked against local state. -/
structure RuntimeInternalEvidence where
  sourceKind : RuntimeInternalSourceKind
  issuerDid : Did
  sourceRequestId : String
  targetNode : Did
  targetRuntimeAttestationValid : Bool
  sourceBindingCurrent : Bool
  triggerConfigDocumentBindingCurrent : Bool
  sourceDocumentBindingCurrent : Bool
  targetPolicyAllows : Bool
  verifiedGoalContinuation : Option GoalContinuationInput := none
  auxiliaryParent : Option AuxiliaryParentEvidence := none
  /-- Read by the session owner; `none` for a new session. -/
  sessionScope : Option Did := none
  deriving DecidableEq, Repr

/-- A completion wake, control continuation or trigger fire delivered into an
existing session preserves its creation requester as provenance; a new unattended
session is owned by the selected agent and inherits configured user access. The runtime can choose no other requester. Local-control
continuations additionally bind a parent in the same session under the same
requester (`sourceDocumentBindingCurrent`). A title audit is audit-only and
never delivered into a session, so its requester is the selected agent. Runtime
issuance is signed by that agent; the Node DID remains routing and peer admission only. -/
def runtimeRequesterScope (request : AgentRequestSemantics)
    (evidence : RuntimeInternalEvidence) : Did :=
  match request.purpose with
  | .titleAudit | .accessReview => request.agentDid
  | .normal => evidence.sessionScope.getD request.agentDid

def exactRuntimeInternalEvidence
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (evidence : RuntimeInternalEvidence) : Prop :=
  evidence.issuerDid = admission.issuerDid ∧
  evidence.sourceRequestId = admission.sourceRequestId ∧
  evidence.sourceKind = admission.runtimeSourceKind ∧
  evidence.targetNode = request.targetNode ∧
  admission.issuerDid = request.agentDid ∧
  admission.signerDid = request.agentDid ∧
  evidence.targetRuntimeAttestationValid = true ∧
  evidence.sourceBindingCurrent = true ∧
  request.requesterDid = runtimeRequesterScope request evidence ∧
  match admission.runtimeSourceKind with
  | .localControl =>
      evidence.sourceDocumentBindingCurrent = true
  | .automatedTrigger =>
      evidence.triggerConfigDocumentBindingCurrent = true ∧
      evidence.targetPolicyAllows = true

instance (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (evidence : RuntimeInternalEvidence) : Decidable
    (exactRuntimeInternalEvidence request admission evidence) := by
  unfold exactRuntimeInternalEvidence
  cases admission.runtimeSourceKind <;> infer_instance

def exactEnrollmentGeneration
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Request) (decision : Decision) : Prop :=
  request.requesterDid = enrollmentRequest.candidateDid ∧
  request.targetNode = enrollmentRequest.ownerNode ∧
  admission.signerDid = request.requesterDid ∧
  admission.enrollmentRequestId = enrollmentRequest.requestId ∧
  admission.enrollmentRequestDigest = enrollmentRequest.digest ∧
  admission.enrollmentAdminDid = enrollmentRequest.adminDid ∧
  admission.enrollmentAuthorizationSequence = decision.authorizationSequence ∧
  admission.enrollmentAuthorizationExpiresAt = decision.authorizationExpiresAt

instance (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Request) (decision : Decision) : Decidable
    (exactEnrollmentGeneration request admission enrollmentRequest decision) := by
  unfold exactEnrollmentGeneration; infer_instance

/-- Neither the parent's lifecycle state nor its execution generation grants
title write authority. -/
def auxiliaryPurposeAllowed
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (runtimeEvidence : Option RuntimeInternalEvidence) : Prop :=
  admission.kind = .runtimeInternal ∧
  admission.runtimeSourceKind = .localControl ∧
  request.requesterDid = request.agentDid ∧
  admission.signerDid = request.agentDid ∧
  admission.issuerDid = request.agentDid ∧
  request.input = {} ∧
  request.retryFields = [] ∧
  request.triggerFields = [] ∧
  request.triggerConfigDocumentId = "" ∧
  match runtimeEvidence with
  | some evidence =>
      match evidence.auxiliaryParent with
      | some parent =>
          parent.link.requestId ≠ "" ∧ parent.link.documentId ≠ "" ∧
          parent.link.requestId ≠ request.requestId ∧
          request.parentFields = auxiliaryParentFields parent.link ∧
          admission.sourceRequestId = parent.link.requestId ∧
          parent.nodeDid = request.targetNode ∧
          parent.sessionId = request.sessionId ∧
          parent.agentDid = request.agentDid ∧
          parent.logicalBindingCurrent = true ∧
          parent.physicalBindingCurrent = true ∧
          evidence.sourceDocumentBindingCurrent = true
      | none => False
  | none => False

def requestPurposeAllowed
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (runtimeEvidence : Option RuntimeInternalEvidence) : Prop :=
  match request.purpose with
  | .normal => True
  | .titleAudit | .accessReview => auxiliaryPurposeAllowed request admission runtimeEvidence

instance (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (runtimeEvidence : Option RuntimeInternalEvidence) :
    Decidable (requestPurposeAllowed request admission runtimeEvidence) := by
  cases hpurpose : request.purpose with
  | normal =>
    simpa [requestPurposeAllowed, hpurpose] using (inferInstance : Decidable True)
  | titleAudit | accessReview =>
    cases runtimeEvidence with
    | none =>
      simpa [requestPurposeAllowed, hpurpose, auxiliaryPurposeAllowed] using
        (inferInstance : Decidable False)
    | some evidence =>
      cases hparent : evidence.auxiliaryParent with
      | none =>
        simp only [requestPurposeAllowed, hpurpose, auxiliaryPurposeAllowed, hparent]
        infer_instance
      | some parent =>
        simp only [requestPurposeAllowed, hpurpose, auxiliaryPurposeAllowed, hparent]
        infer_instance

/-- Native source validation binds the signed physical parent request, its canonical
source session and exact tool-call parent/document/logical identifiers. A target
session creation receipt or finalized delivery receipt may not yet exist when the
first request is claimed, so neither is required. Source Agent DID comes from that
source session, not the
recipient Agent or Node identity. This is provenance validation; target document
permission is still enforced by native ACP, never by same-node membership. -/
structure LocalAgentSourceEvidence where
  sourceNode : Did
  sourceAgent : Did
  sourceSessionAgent : Did
  signedPhysicalSourceCurrent : Bool
  deriving DecidableEq, Repr

def localAgentSourceMatches (request : AgentRequestSemantics)
    (evidence : LocalAgentSourceEvidence) : Prop :=
  evidence.sourceNode = request.targetNode ∧
  evidence.sourceAgent = request.requesterDid ∧
  evidence.sourceSessionAgent = evidence.sourceAgent ∧
  evidence.signedPhysicalSourceCurrent = true

instance (request : AgentRequestSemantics) (evidence : LocalAgentSourceEvidence) :
    Decidable (localAgentSourceMatches request evidence) := by
  unfold localAgentSourceMatches; infer_instance

structure TargetAuthority where
  /-- Canonical existing-session creation provenance. Client admission requires it:
  the client submission transaction ensures the session before publishing its request.
  This is not an access grant; the client actor is the admission signer. -/
  sessionScope : Option Did := none
  localAgentSource : Option LocalAgentSourceEvidence := none
  /-- Evidence of the native target-bound creation transaction: the immutable
  target is exact, the signed sender owns the request and the working agent can
  update it. This is a committed fact, not a P2P enrollment or a fresh
  application-level permission check. Unprotected schemas cannot supply it. -/
  nativeCreateAuthorized : Bool
  maxRequestHop : Option Nat
  deriving DecidableEq, Repr

/-- `authorizationFresh` is the current-clock lease check made during this
admission attempt, not a cached observation. -/
def agentRequestAdmissible
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision)
    (authorizationFresh : Bool) (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool)
    (target : TargetAuthority) : Prop :=
  admission.signatureValid = true ∧
  admission.signedFields = agentRequestAdmissionFields request admission ∧
  branchFieldsExact = true ∧
  pendingDeadlineAbsent = true ∧
  (match admission.kind with
  | .enrollment =>
      match enrollmentRequest, decision with
      | some enrolledRequest, some approval =>
          currentApproval s enrolledRequest approval ∧
          exactEnrollmentGeneration request admission enrolledRequest approval ∧
          authorizationFresh = true
      | _, _ => False
  | .client => target.sessionScope = some request.requesterDid
  | .localSelf =>
      admission.signerDid = request.requesterDid ∧
      (request.requesterDid = request.agentDid ∨
        match target.localAgentSource with
        | some evidence => localAgentSourceMatches request evidence
        | none => False)
  | .peer =>
      admission.signerDid = request.requesterDid ∧
      request.requesterDid ≠ request.agentDid ∧
      target.nativeCreateAuthorized = true
  | .runtimeInternal =>
      match runtimeEvidence with
      | some evidence => exactRuntimeInternalEvidence request admission evidence
      | none => False) ∧
  requestPurposeAllowed request admission runtimeEvidence ∧
  CausalHop.admitHop target.maxRequestHop request.hop = true

instance (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision)
    (authorizationFresh : Bool) (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool) (target : TargetAuthority) :
    Decidable (agentRequestAdmissible s request admission enrollmentRequest decision
      authorizationFresh runtimeEvidence branchFieldsExact pendingDeadlineAbsent
      target) := by
  rcases target with ⟨sessionScope, source, peer, hop⟩
  cases source <;> unfold agentRequestAdmissible
  all_goals cases admission.kind <;> cases enrollmentRequest <;> cases decision <;>
    cases runtimeEvidence <;> infer_instance

theorem client_admission_independent_of_enrollment
    (s t : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (er er' : Option Request) (decision decision' : Option Decision)
    (fresh fresh' : Bool) (evidence : Option RuntimeInternalEvidence)
    (fields deadline : Bool) (target : TargetAuthority)
    (hkind : admission.kind = .client) :
    agentRequestAdmissible s request admission er decision fresh evidence fields deadline target ↔
    agentRequestAdmissible t request admission er' decision' fresh' evidence fields deadline target := by
  simp only [agentRequestAdmissible, hkind]

/-- This is only a routing candidate for a cross-actor Client claim. Native
claim authorization, session binding and the verified Goal edge remain required;
queue metadata cannot supply any of them. -/
def clientClaimQueueCandidate (input : RequestInput) (localAgentMessage : Bool) : Bool :=
  match input.queue with
  | none => true
  | some queue =>
      queue.source == .user ||
      (localAgentMessage && queue.source == .steering) ||
      (queue.source == .goal && input.goalContinuation.isSome)

example : clientClaimQueueCandidate
    { queue := some { source := .goal, policy := .coalesce },
      goalContinuation := some ⟨1, false⟩ } false = true := by decide

example : clientClaimQueueCandidate
    { queue := some { source := .goal, policy := .coalesce } } false = false := by decide

/-- A Client Goal continuation needs the current visible physical Goal and
its physical assignment opening. These facts are read in the selected Agent's
claim transaction; a missing or hidden Goal does not grant authority. -/
def clientGoalClaimBinding (goalVisible goalMatches rootBound : Bool) : Bool :=
  goalVisible && goalMatches && rootBound

theorem client_goal_claim_requires_physical_binding
    (visible goalMatches root : Bool) (h : clientGoalClaimBinding visible goalMatches root = true) :
    visible = true ∧ goalMatches = true ∧ root = true := by
  simpa [clientGoalClaimBinding, Bool.and_eq_true, and_assoc] using h

/-- Queue selection is only one input to a Client Goal claim. The signed edge,
canonical physical Goal binding and current native authorization all remain
necessary at the claim transaction. -/
def clientGoalClaimAllowed (input : RequestInput) (signatureValid sessionAgentBound
    nativeAuthorAllowed goalVisible goalMatches rootBound ancestryVerified : Bool) : Bool :=
  input.goalContinuation.isSome && clientClaimQueueCandidate input false && signatureValid && sessionAgentBound &&
    nativeAuthorAllowed && clientGoalClaimBinding goalVisible goalMatches rootBound &&
    ancestryVerified

theorem client_goal_claim_requires_current_author_and_ancestry (input : RequestInput)
    (signature sessionAgent nativeAccess visible goalMatches root ancestry : Bool)
    (h : clientGoalClaimAllowed input signature sessionAgent nativeAccess visible goalMatches root ancestry = true) :
    nativeAccess = true ∧ ancestry = true := by
  simp only [clientGoalClaimAllowed, Bool.and_eq_true] at h
  aesop

/-- Queue provenance never grants a continuation branch. An explicit client
resume keeps the client signer and must bind the same canonical goal edge as a
runtime timer. A goal wake missing its original facts cannot infer them from Goal.
Client continuations permit only the goal queue source, never unrelated runtime
queue sources; `inputWithinContext` retains that independent source check. -/
def goalContinuationAllowed (input : RequestInput) (kind : AgentRequestAdmissionKind)
    (source : RuntimeInternalSourceKind) (verified : Option GoalContinuationInput) : Bool :=
  match input.goalContinuation with
  | none => !(input.queue.any (fun q => q.source == .goal))
  | some facts =>
      facts.sequence > 0 &&
        ((kind == .runtimeInternal && source == .localControl) || kind == .client) &&
        verified == some facts

def requestGoalInputAllowed (request : AgentRequestSemantics)
    (admission : AgentRequestAdmission) (evidence : Option RuntimeInternalEvidence) : Bool :=
  goalContinuationAllowed request.input admission.kind admission.runtimeSourceKind
    (evidence.bind RuntimeInternalEvidence.verifiedGoalContinuation)

def agentRequestClaimable
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision)
    (authorizationFresh : Bool) (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool) (target : TargetAuthority)
    (sessionAgent : String) (skillIds : List String)
    (cwdAllowed : String → Bool) (queueSourceAllowed : SessionQueue.QueueSource → Bool) : Prop :=
  agentMatchesSession request.agentDid sessionAgent = true ∧
  inputWithinContext request.input skillIds cwdAllowed queueSourceAllowed = true ∧
  agentRequestAdmissible s request admission enrollmentRequest decision authorizationFresh
    runtimeEvidence branchFieldsExact pendingDeadlineAbsent target ∧
  requestGoalInputAllowed request admission runtimeEvidence = true

instance (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision)
    (authorizationFresh : Bool) (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool) (target : TargetAuthority)
    (sessionAgent : String) (skillIds : List String)
    (cwdAllowed : String → Bool) (queueSourceAllowed : SessionQueue.QueueSource → Bool) :
    Decidable (agentRequestClaimable s request admission enrollmentRequest decision
      authorizationFresh runtimeEvidence branchFieldsExact pendingDeadlineAbsent
      target sessionAgent skillIds cwdAllowed queueSourceAllowed) := by
  unfold agentRequestClaimable
  infer_instance

theorem claim_requires_authenticated_input
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision}
    {fresh : Bool} {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool} {target : TargetAuthority}
    {sessionAgent : String} {skills : List String}
    {cwdAllowed : String → Bool} {queueAllowed : SessionQueue.QueueSource → Bool}
    (h : agentRequestClaimable s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent target
      sessionAgent skills cwdAllowed queueAllowed) :
    admission.signatureValid = true ∧
    admission.signedFields = agentRequestAdmissionFields request admission ∧
    agentMatchesSession request.agentDid sessionAgent = true ∧
    inputWithinContext request.input skills cwdAllowed queueAllowed = true := by
  exact ⟨h.2.2.1.1, h.2.2.1.2.1, h.1, h.2.1⟩

theorem title_requires_runtime_parent_only
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool} {target : TargetAuthority}
    (hpurpose : request.purpose = .titleAudit)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent target) :
    admission.kind = .runtimeInternal ∧
    admission.runtimeSourceKind = .localControl ∧
    request.requesterDid = request.agentDid ∧
    admission.signerDid = request.agentDid ∧
    request.input = {} ∧
    ∃ evidence parent,
      runtimeEvidence = some evidence ∧ evidence.auxiliaryParent = some parent ∧
      request.parentFields = auxiliaryParentFields parent.link ∧
      parent.nodeDid = request.targetNode ∧
      parent.sessionId = request.sessionId ∧
      parent.agentDid = request.agentDid := by
  have htitle : auxiliaryPurposeAllowed request admission runtimeEvidence := by
    simpa [requestPurposeAllowed, hpurpose] using hadmit.2.2.2.2.2.1
  rcases runtimeEvidence with _ | evidence
  · simp [auxiliaryPurposeAllowed] at htitle
  cases hparent : evidence.auxiliaryParent with
  | none => simp [auxiliaryPurposeAllowed, hparent] at htitle
  | some parent =>
    simp only [auxiliaryPurposeAllowed, hparent] at htitle
    rcases htitle with ⟨hkind, hsource, hrequester, hsigner, _, hinput, _, _, _,
      _, _, _, hfields, _, hagent, hsession, hagentSelected, _, _, _⟩
    exact ⟨hkind, hsource, hrequester, hsigner, hinput,
      evidence, parent, rfl, hparent, hfields, hagent, hsession, hagentSelected⟩

theorem goal_input_requires_verified_control (input : RequestInput)
    (facts : GoalContinuationInput) (kind : AgentRequestAdmissionKind)
    (source : RuntimeInternalSourceKind) (verified : Option GoalContinuationInput)
    (hpresent : input.goalContinuation = some facts)
    (h : goalContinuationAllowed input kind source verified = true) :
    facts.sequence > 0 ∧
      ((kind = .runtimeInternal ∧ source = .localControl) ∨ kind = .client) ∧
      verified = some facts := by
  simpa [goalContinuationAllowed, hpresent, Bool.and_eq_true, Bool.or_eq_true,
    and_assoc] using h

structure AgentRequestAdmissionObservation where
  kind : AgentRequestAdmissionKind
  signatureValid : Bool
  signedFieldsMatch : Bool
  branchFieldsExact : Bool
  pendingDeadlineAbsent : Bool
  signerMatchesRequester : Bool
  requesterMatchesTarget : Bool
  signerMatchesTarget : Bool
  signerMatchesIssuer : Bool
  /-- For Client this also proves the canonical session exists. New-session planning
  precedes atomic publication and is not runtime admission. -/
  requesterMatchesSessionScope : Bool
  currentApproval : Bool
  exactGeneration : Bool
  authorizationFresh : Bool
  runtimeEvidencePresent : Bool
  runtimeSourceKind : RuntimeInternalSourceKind
  targetRuntimeAttestationValid : Bool
  sourceBindingCurrent : Bool
  triggerConfigDocumentBindingCurrent : Bool
  sourceDocumentBindingCurrent : Bool
  targetPolicyAllows : Bool
  nativeCreateAuthorized : Bool
  hopWithinBound : Bool
  deriving DecidableEq, Repr

def projectAgentRequestAdmission (observation : AgentRequestAdmissionObservation) : Bool :=
  observation.signatureValid && observation.signedFieldsMatch && observation.branchFieldsExact &&
  observation.pendingDeadlineAbsent && observation.hopWithinBound &&
  match observation.kind with
  | .enrollment =>
      observation.signerMatchesRequester && observation.currentApproval &&
      observation.exactGeneration && observation.authorizationFresh
  | .client => observation.requesterMatchesSessionScope
  | .localSelf =>
      observation.signerMatchesRequester &&
      (observation.requesterMatchesTarget || observation.sourceBindingCurrent)
  | .peer =>
      observation.signerMatchesRequester &&
      !observation.requesterMatchesTarget &&
      observation.nativeCreateAuthorized
  | .runtimeInternal =>
      observation.runtimeEvidencePresent &&
      observation.signerMatchesIssuer && observation.signerMatchesTarget &&
      observation.targetRuntimeAttestationValid && observation.sourceBindingCurrent &&
      match observation.runtimeSourceKind with
      | .localControl =>
          observation.requesterMatchesSessionScope &&
          observation.sourceDocumentBindingCurrent
      | .automatedTrigger =>
          observation.requesterMatchesSessionScope &&
          observation.triggerConfigDocumentBindingCurrent && observation.targetPolicyAllows

/-- A historical local-self request with no requester cannot match its nonempty
Agent signer, even though optional scope comparison remains meaningful for retained data. -/
theorem local_self_without_requester_is_denied
    (observation : AgentRequestAdmissionObservation)
    (kind : observation.kind = .localSelf)
    (missing : observation.signerMatchesRequester = false) :
    projectAgentRequestAdmission observation = false := by
  simp [projectAgentRequestAdmission, kind, missing]

/-- The claim boundary distinguishes an authoritative negative observation from an
unavailable observation. Only the former may terminalize a durable request. -/
inductive AgentRequestAdmissionDisposition where
  | admit
  | deny
  | retry
  deriving DecidableEq, Repr

def admissionDispositionFromResult
    (observationAvailable admitted : Bool) : AgentRequestAdmissionDisposition :=
  if !observationAvailable then .retry
  else if admitted then .admit
  else .deny

def projectAgentRequestAdmissionDisposition
    (observationAvailable : Bool) (observation : AgentRequestAdmissionObservation) :
    AgentRequestAdmissionDisposition :=
  admissionDispositionFromResult observationAvailable
    (projectAgentRequestAdmission observation)

def auxiliaryPendingDisposition
    (observationAvailable : Bool) (s : State) (request : AgentRequestSemantics)
    (admission : AgentRequestAdmission) (runtimeEvidence : Option RuntimeInternalEvidence)
    (sessionAgent : String) (branchFieldsExact pendingDeadlineAbsent : Bool)
    (maxRequestHop : Option Nat) :
    AgentRequestAdmissionDisposition :=
  if !request.purpose.isAuxiliary then .deny
  else admissionDispositionFromResult observationAvailable <|
    decide (agentRequestClaimable s request admission none none false runtimeEvidence
      branchFieldsExact pendingDeadlineAbsent { nativeCreateAuthorized := false, maxRequestHop := maxRequestHop } sessionAgent []
      (fun _ => false) (fun _ => false))

def auxiliaryPendingStep?
    (observationAvailable : Bool) (s : State) (request : AgentRequestSemantics)
    (admission : AgentRequestAdmission) (runtimeEvidence : Option RuntimeInternalEvidence)
    (sessionAgent : String) (branchFieldsExact pendingDeadlineAbsent : Bool)
    (maxRequestHop : Option Nat) (pending : RequestContext) : Option RequestContext :=
  if !request.purpose.isAuxiliary || pending.state != .pending ||
      pending.admission != .released then none
  else
    match auxiliaryPendingDisposition observationAvailable s request admission runtimeEvidence
        sessionAgent branchFieldsExact pendingDeadlineAbsent maxRequestHop with
    | .admit => some pending
    | .deny => pending.step? .admissionReject
    | .retry => some pending

theorem unavailable_admission_observation_retries
    (observation : AgentRequestAdmissionObservation) :
    projectAgentRequestAdmissionDisposition false observation = .retry := by
  simp [projectAgentRequestAdmissionDisposition, admissionDispositionFromResult]

theorem available_negative_admission_observation_denies
    (observation : AgentRequestAdmissionObservation)
    (hdeny : projectAgentRequestAdmission observation = false) :
    projectAgentRequestAdmissionDisposition true observation = .deny := by
  simp [projectAgentRequestAdmissionDisposition, admissionDispositionFromResult, hdeny]

theorem unavailable_title_pending_preserves
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (evidence : Option RuntimeInternalEvidence) (agent: String)
    (branchFieldsExact pendingDeadlineAbsent : Bool) (maxRequestHop : Option Nat)
    (pending : RequestContext)
    (hpurpose : request.purpose = .titleAudit)
    (hstate : pending.state = .pending) (hslot : pending.admission = .released) :
    auxiliaryPendingStep? false s request admission evidence agent branchFieldsExact
      pendingDeadlineAbsent maxRequestHop pending = some pending := by
  simp [auxiliaryPendingStep?, auxiliaryPendingDisposition, admissionDispositionFromResult,
    hpurpose, RequestPurpose.isAuxiliary, hstate, hslot]

theorem admitted_title_pending_awaits_owned_claim
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (evidence : Option RuntimeInternalEvidence) (agent: String)
    (branchFieldsExact pendingDeadlineAbsent : Bool) (maxRequestHop : Option Nat)
    (pending : RequestContext)
    (hpurpose : request.purpose = .titleAudit)
    (hstate : pending.state = .pending) (hslot : pending.admission = .released)
    (hadmit : auxiliaryPendingDisposition true s request admission evidence agent
      branchFieldsExact pendingDeadlineAbsent maxRequestHop = .admit) :
    auxiliaryPendingStep? true s request admission evidence agent branchFieldsExact
      pendingDeadlineAbsent maxRequestHop pending = some pending := by
  simp [auxiliaryPendingStep?, hpurpose, RequestPurpose.isAuxiliary, hstate, hslot, hadmit]

theorem denied_title_pending_uses_admission_reject
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (evidence : Option RuntimeInternalEvidence) (agent: String)
    (branchFieldsExact pendingDeadlineAbsent : Bool) (maxRequestHop : Option Nat)
    (pending : RequestContext)
    (hpurpose : request.purpose = .titleAudit)
    (hstate : pending.state = .pending) (hslot : pending.admission = .released)
    (hdeny : auxiliaryPendingDisposition true s request admission evidence agent
      branchFieldsExact pendingDeadlineAbsent maxRequestHop = .deny) :
    auxiliaryPendingStep? true s request admission evidence agent branchFieldsExact
      pendingDeadlineAbsent maxRequestHop pending =
      pending.step? .admissionReject := by
  simp [auxiliaryPendingStep?, hpurpose, RequestPurpose.isAuxiliary, hstate, hslot, hdeny]

theorem enrollment_requires_current_exact_generation
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool} {target : TargetAuthority}
    (hkind : admission.kind = .enrollment)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent target) :
    ∃ enrolledRequest approval,
      enrollmentRequest = some enrolledRequest ∧ decision = some approval ∧
      currentApproval s enrolledRequest approval ∧
      exactEnrollmentGeneration request admission enrolledRequest approval ∧ fresh = true := by
  simp only [agentRequestAdmissible, hkind] at hadmit
  rcases enrollmentRequest with _ | enrolledRequest <;> rcases decision with _ | approval <;>
    simp_all

theorem enrollment_expiry_fails_closed
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision)
    (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool) (target : TargetAuthority)
    (hkind : admission.kind = .enrollment) :
    ¬ agentRequestAdmissible s request admission enrollmentRequest decision false runtimeEvidence
      branchFieldsExact pendingDeadlineAbsent target := by
  rcases enrollmentRequest with _ | enrolledRequest <;> rcases decision with _ | approval <;>
    simp [agentRequestAdmissible, hkind]

theorem local_self_requires_selected_agent_or_exact_source
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool} {target : TargetAuthority}
    (hkind : admission.kind = .localSelf)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent target) :
    admission.signerDid = request.requesterDid ∧
      (request.requesterDid = request.agentDid ∨
        match target.localAgentSource with
        | some evidence => localAgentSourceMatches request evidence
        | none => False) := by
  simp only [agentRequestAdmissible, hkind] at hadmit
  rcases hadmit with ⟨_, _, _, _, ⟨hsigner, htarget⟩, _⟩
  exact ⟨hsigner, htarget⟩

theorem local_self_admission_is_independent_of_enrollment_state
    (s₁ s₂ : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision) (fresh : Bool)
    (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool) (target : TargetAuthority)
    (hkind : admission.kind = .localSelf) :
    agentRequestAdmissible s₁ request admission enrollmentRequest decision fresh runtimeEvidence
        branchFieldsExact pendingDeadlineAbsent target ↔
      agentRequestAdmissible s₂ request admission enrollmentRequest decision fresh runtimeEvidence
        branchFieldsExact pendingDeadlineAbsent target := by
  simp [agentRequestAdmissible, hkind]

theorem runtime_internal_requires_owned_issue
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool} {target : TargetAuthority}
    (hkind : admission.kind = .runtimeInternal)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent target) :
    ∃ evidence, runtimeEvidence = some evidence ∧
      exactRuntimeInternalEvidence request admission evidence := by
  simp only [agentRequestAdmissible, hkind] at hadmit
  rcases runtimeEvidence with _ | evidence <;> simp_all

theorem runtime_internal_adopts_session_scope
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool} {target : TargetAuthority}
    (hkind : admission.kind = .runtimeInternal) (hnormal : request.purpose = .normal)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent target) :
    ∃ evidence, runtimeEvidence = some evidence ∧
      request.requesterDid = evidence.sessionScope.getD request.agentDid := by
  obtain ⟨evidence, hsome, hexact⟩ := runtime_internal_requires_owned_issue hkind hadmit
  refine ⟨evidence, hsome, ?_⟩
  have h := hexact.2.2.2.2.2.2.2.2.1
  simpa [runtimeRequesterScope, hnormal] using h

theorem runtime_internal_admission_is_independent_of_enrollment_state
    (s₁ s₂ : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision) (fresh : Bool)
    (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool) (target : TargetAuthority)
    (hkind : admission.kind = .runtimeInternal) :
    agentRequestAdmissible s₁ request admission enrollmentRequest decision fresh runtimeEvidence
        branchFieldsExact pendingDeadlineAbsent target ↔
      agentRequestAdmissible s₂ request admission enrollmentRequest decision fresh runtimeEvidence
        branchFieldsExact pendingDeadlineAbsent target := by
  simp [agentRequestAdmissible, hkind]

theorem peer_requires_foreign_signed_authorized_requester
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool} {target : TargetAuthority}
    (hkind : admission.kind = .peer)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent target) :
    admission.signerDid = request.requesterDid ∧
      request.requesterDid ≠ request.agentDid ∧ target.nativeCreateAuthorized = true := by
  simp only [agentRequestAdmissible, hkind] at hadmit
  exact hadmit.2.2.2.2.1

theorem admitted_request_within_configured_hop_bound
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool} {target : TargetAuthority}
    {bound : Nat} (hbound : target.maxRequestHop = some bound)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent target) :
    request.hop ≤ bound := by
  simpa [CausalHop.admitHop, hbound] using hadmit.2.2.2.2.2.2

end Enrollment
