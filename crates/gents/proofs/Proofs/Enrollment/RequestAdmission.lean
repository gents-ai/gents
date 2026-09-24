import Proofs.Enrollment.Transition
import Proofs.Enrollment.RequestInput
import Proofs.Request.Executable

/-!
# Agent request admission owned by authenticated enrollment

Replication is transport, not authority.  Every newly observed `AgentRequest`
must carry exactly one authenticated provenance branch.  Mutable lifecycle
columns are deliberately outside `AgentRequestSemantics`; the signature covers
every immutable request semantic and every admission-generation field.
-/

namespace Enrollment

inductive AgentRequestAdmissionKind where
  | enrollment
  | localSelf
  | runtimeInternal
  deriving DecidableEq, Repr

inductive RuntimeInternalSourceKind where
  | localChild
  | crossPrincipalChild
  | localControl
  | automatedTrigger
  deriving DecidableEq, Repr

/-- Parent-only provenance is signed by the title request, but is not a
delegation edge or authority to write under the parent request. -/
structure TitleParentLink where
  requestId : String
  documentId : String
  deriving DecidableEq, Repr

/-- The existing local-control source lookup authenticates these facts against
the parent row. Its lifecycle state is intentionally not an admission input. -/
structure TitleParentEvidence where
  link : TitleParentLink
  agentDid : Did
  sessionId : String
  behaviorId : String
  logicalBindingCurrent : Bool
  physicalBindingCurrent : Bool
  deriving DecidableEq, Repr

/-- Abstract signed semantic fields. Native `push_option` uses byte tags 1/0;
the native adapter must map the typed link through its existing encoder, not
interpret these model labels as the native signature payload bytes. -/
def titleParentFields (link : TitleParentLink) : CanonicalFields :=
  textFieldsToBytes ["0", "some", link.requestId, "some", link.documentId, "none", "none"]

/-- Exact immutable request semantics covered by the request signature. -/
structure AgentRequestSemantics where
  requestId : String
  purpose : RequestPurpose
  targetAgent : Did
  requesterDid : Did
  behaviorId : String
  sessionId : String
  content : String
  input : RequestInput
  createdAt : String
  /-- Physical `_docID` of the canonical Trigger configuration row. -/
  triggerConfigDocumentId : String
  retryFields : CanonicalFields
  triggerFields : CanonicalFields
  parentFields : CanonicalFields
  workspace : RequestWorkspace
  deriving DecidableEq, Repr

def agentRequestSemanticFields (request : AgentRequestSemantics) : CanonicalFields :=
  textFieldsToBytes
    [ request.requestId, request.purpose.toWire, request.targetAgent, request.requesterDid
    , request.behaviorId, request.sessionId, request.content ] ++
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
  bridgeAuthorDid : Did
  signedFields : CanonicalFields
  signatureValid : Bool
  deriving DecidableEq, Repr

def agentRequestAdmissionFields
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission) : CanonicalFields :=
  agentRequestSemanticFields request ++ textFieldsToBytes
    [ match admission.kind with
      | .enrollment => "enrollment"
      | .localSelf => "local-self"
      | .runtimeInternal => "runtime-internal"
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
          | .localChild => "local-child"
          | .crossPrincipalChild => "cross-principal-child"
          | .localControl => "local-control"
          | .automatedTrigger => "automated-trigger"
      | _ => ""
    , admission.bridgeAuthorDid ]

theorem purpose_change_changes_signed_fields
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (hpurpose : request.purpose = .normal) :
    agentRequestAdmissionFields request admission ≠
      agentRequestAdmissionFields { request with purpose := .titleAudit } admission := by
  intro heq
  have hsecond := congrArg (fun fields => fields[1]?) heq
  simp [agentRequestAdmissionFields, agentRequestSemanticFields, textFieldsToBytes,
    hpurpose, RequestPurpose.toWire] at hsecond
  exact (by decide : stringBytes "normal" ≠ stringBytes "title-audit") hsecond

/--
Claim-time evidence reconstructed by the target runtime.  These observations
are never trusted from request columns: the issuer signature, durable source
binding, and target policy are checked against local state.
-/
structure RuntimeInternalEvidence where
  sourceKind : RuntimeInternalSourceKind
  issuerDid : Did
  sourceRequestId : String
  bridgeAuthorDid : Did
  targetAgent : Did
  targetRuntimeAttestationValid : Bool
  /-- Includes complete signed workspace reference agreement with the existing
  authenticated source via requestWorkspaceWithinSource; cross-principal sources
  are exact ACP-authenticated bridges, not a parent-replication requirement. -/
  sourceBindingCurrent : Bool
  triggerConfigDocumentBindingCurrent : Bool
  sourceDocumentBindingCurrent : Bool
  sourceToolCallBindingCurrent : Bool
  targetPolicyAllows : Bool
  bridgeAuthorBindingCurrent : Bool
  bridgeAuthorAuthorizationFresh : Bool
  targetCrossPrincipalPolicyAllows : Bool
  /-- Existing Goal physical-edge validator authenticates this exact receipt's
  original sequence/wrapup, source/goal/physical-parent binding and deterministic
  identity. This is reconstructed evidence, never copied from input unchecked. -/
  verifiedGoalContinuation : Option GoalContinuationInput := none
  titleParent : Option TitleParentEvidence := none
  deriving DecidableEq, Repr

def exactRuntimeInternalEvidence
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (evidence : RuntimeInternalEvidence) : Prop :=
  evidence.issuerDid = admission.issuerDid ∧
  evidence.sourceRequestId = admission.sourceRequestId ∧
  evidence.sourceKind = admission.runtimeSourceKind ∧
  evidence.targetAgent = request.targetAgent ∧
  admission.issuerDid = request.targetAgent ∧
  admission.signerDid = request.targetAgent ∧
  evidence.targetRuntimeAttestationValid = true ∧
  evidence.sourceBindingCurrent = true ∧
  match admission.runtimeSourceKind with
  | .localChild =>
      request.requesterDid = request.targetAgent ∧
      admission.bridgeAuthorDid = "" ∧ evidence.bridgeAuthorDid = "" ∧
      evidence.sourceDocumentBindingCurrent = true ∧
      evidence.sourceToolCallBindingCurrent = true ∧
      evidence.targetPolicyAllows = true
  | .crossPrincipalChild =>
      admission.bridgeAuthorDid ≠ "" ∧
      request.requesterDid = admission.bridgeAuthorDid ∧
      evidence.bridgeAuthorDid = admission.bridgeAuthorDid ∧
      evidence.sourceToolCallBindingCurrent = true ∧
      evidence.bridgeAuthorBindingCurrent = true ∧
      evidence.bridgeAuthorAuthorizationFresh = true ∧
      evidence.targetCrossPrincipalPolicyAllows = true
  | .localControl =>
      request.requesterDid = request.targetAgent ∧
      admission.bridgeAuthorDid = "" ∧ evidence.bridgeAuthorDid = "" ∧
      evidence.sourceDocumentBindingCurrent = true
  | .automatedTrigger =>
      request.requesterDid = request.targetAgent ∧
      admission.bridgeAuthorDid = "" ∧ evidence.bridgeAuthorDid = "" ∧
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
  request.targetAgent = enrollmentRequest.ownerAgent ∧
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

/-- Title is a runtime-authored, parent-only request in the same session and
behavior. The authenticated parent is provenance; neither its lifecycle state
nor its execution generation grants title write authority. -/
def titlePurposeAllowed
    (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (runtimeEvidence : Option RuntimeInternalEvidence) : Prop :=
  admission.kind = .runtimeInternal ∧
  admission.runtimeSourceKind = .localControl ∧
  request.requesterDid = request.targetAgent ∧
  admission.signerDid = request.targetAgent ∧
  admission.issuerDid = request.targetAgent ∧
  request.input = {} ∧
  request.retryFields = [] ∧
  request.triggerFields = [] ∧
  request.triggerConfigDocumentId = "" ∧
  match runtimeEvidence with
  | some evidence =>
      match evidence.titleParent with
      | some parent =>
          parent.link.requestId ≠ "" ∧ parent.link.documentId ≠ "" ∧
          parent.link.requestId ≠ request.requestId ∧
          request.parentFields = titleParentFields parent.link ∧
          admission.sourceRequestId = parent.link.requestId ∧
          parent.agentDid = request.targetAgent ∧
          parent.sessionId = request.sessionId ∧
          parent.behaviorId = request.behaviorId ∧
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
  | .titleAudit => titlePurposeAllowed request admission runtimeEvidence

instance (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (runtimeEvidence : Option RuntimeInternalEvidence) :
    Decidable (requestPurposeAllowed request admission runtimeEvidence) := by
  cases hpurpose : request.purpose with
  | normal =>
    simpa [requestPurposeAllowed, hpurpose] using (inferInstance : Decidable True)
  | titleAudit =>
    cases runtimeEvidence with
    | none =>
      simpa [requestPurposeAllowed, hpurpose, titlePurposeAllowed] using
        (inferInstance : Decidable False)
    | some evidence =>
      cases hparent : evidence.titleParent with
      | none =>
        simp only [requestPurposeAllowed, hpurpose, titlePurposeAllowed, hparent]
        infer_instance
      | some parent =>
        simp only [requestPurposeAllowed, hpurpose, titlePurposeAllowed, hparent]
        infer_instance

/--
The executable router boundary. `authorizationFresh` is the current-clock
lease check made during this admission attempt, not a cached observation.
-/
def agentRequestAdmissible
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision)
    (authorizationFresh : Bool) (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool) : Prop :=
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
  | .localSelf =>
      admission.signerDid = request.requesterDid ∧
      request.requesterDid = request.targetAgent
  | .runtimeInternal =>
      match runtimeEvidence with
      | some evidence => exactRuntimeInternalEvidence request admission evidence
      | none => False) ∧
  requestPurposeAllowed request admission runtimeEvidence

instance (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision)
    (authorizationFresh : Bool) (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool) :
    Decidable (agentRequestAdmissible s request admission enrollmentRequest decision
      authorizationFresh runtimeEvidence branchFieldsExact pendingDeadlineAbsent) := by
  unfold agentRequestAdmissible
  cases admission.kind <;> cases enrollmentRequest <;> cases decision <;>
    cases runtimeEvidence <;> infer_instance

/-- Runtime-only continuation facts require the authenticated local-control
branch and exact original receipt facts. Queue provenance never grants that
branch; a goal wake missing its original facts cannot infer them from Goal. -/
def goalContinuationAllowed (input : RequestInput) (kind : AgentRequestAdmissionKind)
    (source : RuntimeInternalSourceKind) (verified : Option GoalContinuationInput) : Bool :=
  match input.goalContinuation with
  | none => !(input.queue.any (fun q => q.source == .goal))
  | some facts =>
      facts.sequence > 0 && kind == .runtimeInternal && source == .localControl &&
        verified == some facts

def requestGoalInputAllowed (request : AgentRequestSemantics)
    (admission : AgentRequestAdmission) (evidence : Option RuntimeInternalEvidence) : Bool :=
  goalContinuationAllowed request.input admission.kind admission.runtimeSourceKind
    (evidence.bind RuntimeInternalEvidence.verifiedGoalContinuation)

/-- Composed claim boundary: signatures authenticate input but do not grant
skills, cwd escape, or runtime queue origins. The observed session behavior is
resolved by the session owner; blank/mismatched selection cannot be repaired by
falling back to the principal name. New-session creation passes its selected
behavior through the same boundary. -/
def agentRequestClaimable
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision)
    (authorizationFresh : Bool) (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool)
    (sessionBehavior : String) (skillIds : List String)
    (cwdAllowed : String → Bool) (queueSourceAllowed : SessionQueue.QueueSource → Bool) : Prop :=
  behaviorMatchesSession request.behaviorId sessionBehavior = true ∧
  inputWithinContext request.input skillIds cwdAllowed queueSourceAllowed = true ∧
  agentRequestAdmissible s request admission enrollmentRequest decision authorizationFresh
    runtimeEvidence branchFieldsExact pendingDeadlineAbsent ∧
  requestGoalInputAllowed request admission runtimeEvidence = true

instance (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision)
    (authorizationFresh : Bool) (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool)
    (sessionBehavior : String) (skillIds : List String)
    (cwdAllowed : String → Bool) (queueSourceAllowed : SessionQueue.QueueSource → Bool) :
    Decidable (agentRequestClaimable s request admission enrollmentRequest decision
      authorizationFresh runtimeEvidence branchFieldsExact pendingDeadlineAbsent
      sessionBehavior skillIds cwdAllowed queueSourceAllowed) := by
  unfold agentRequestClaimable
  infer_instance

theorem claim_requires_authenticated_input
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision}
    {fresh : Bool} {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool}
    {sessionBehavior : String} {skills : List String}
    {cwdAllowed : String → Bool} {queueAllowed : SessionQueue.QueueSource → Bool}
    (h : agentRequestClaimable s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent sessionBehavior skills
      cwdAllowed queueAllowed) :
    admission.signatureValid = true ∧
    admission.signedFields = agentRequestAdmissionFields request admission ∧
    behaviorMatchesSession request.behaviorId sessionBehavior = true ∧
    inputWithinContext request.input skills cwdAllowed queueAllowed = true := by
  exact ⟨h.2.2.1.1, h.2.2.1.2.1, h.1, h.2.1⟩

theorem title_requires_runtime_parent_only
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool}
    (hpurpose : request.purpose = .titleAudit)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent) :
    admission.kind = .runtimeInternal ∧
    admission.runtimeSourceKind = .localControl ∧
    request.requesterDid = request.targetAgent ∧
    admission.signerDid = request.targetAgent ∧
    request.input = {} ∧
    ∃ evidence parent,
      runtimeEvidence = some evidence ∧ evidence.titleParent = some parent ∧
      request.parentFields = titleParentFields parent.link ∧
      parent.agentDid = request.targetAgent ∧
      parent.sessionId = request.sessionId ∧
      parent.behaviorId = request.behaviorId := by
  have htitle : titlePurposeAllowed request admission runtimeEvidence := by
    simpa [requestPurposeAllowed, hpurpose] using hadmit.2.2.2.2.2
  rcases runtimeEvidence with _ | evidence
  · simp [titlePurposeAllowed] at htitle
  cases hparent : evidence.titleParent with
  | none => simp [titlePurposeAllowed, hparent] at htitle
  | some parent =>
    simp only [titlePurposeAllowed, hparent] at htitle
    rcases htitle with ⟨hkind, hsource, hrequester, hsigner, _, hinput, _, _, _,
      _, _, _, hfields, _, hagent, hsession, hbehavior, _, _, _⟩
    exact ⟨hkind, hsource, hrequester, hsigner, hinput,
      evidence, parent, rfl, hparent, hfields, hagent, hsession, hbehavior⟩

theorem goal_input_requires_runtime_control (input : RequestInput)
    (facts : GoalContinuationInput) (kind : AgentRequestAdmissionKind)
    (source : RuntimeInternalSourceKind) (verified : Option GoalContinuationInput)
    (hpresent : input.goalContinuation = some facts)
    (h : goalContinuationAllowed input kind source verified = true) :
    facts.sequence > 0 ∧ kind = .runtimeInternal ∧ source = .localControl ∧
      verified = some facts := by
  simpa [goalContinuationAllowed, hpresent, Bool.and_eq_true, and_assoc] using h

/-- Explicit observations used by generated implementation conformance cases. -/
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
  requesterMatchesIssuer : Bool
  requesterMatchesBridgeAuthor : Bool
  currentApproval : Bool
  exactGeneration : Bool
  authorizationFresh : Bool
  runtimeEvidencePresent : Bool
  runtimeSourceKind : RuntimeInternalSourceKind
  targetRuntimeAttestationValid : Bool
  sourceBindingCurrent : Bool
  triggerConfigDocumentBindingCurrent : Bool
  sourceDocumentBindingCurrent : Bool
  sourceToolCallBindingCurrent : Bool
  targetPolicyAllows : Bool
  bridgeAuthorBindingCurrent : Bool
  bridgeAuthorAuthorizationFresh : Bool
  targetCrossPrincipalPolicyAllows : Bool
  deriving DecidableEq, Repr

def projectAgentRequestAdmission (observation : AgentRequestAdmissionObservation) : Bool :=
  observation.signatureValid && observation.signedFieldsMatch && observation.branchFieldsExact &&
  observation.pendingDeadlineAbsent &&
  match observation.kind with
  | .enrollment =>
      observation.signerMatchesRequester && observation.currentApproval &&
      observation.exactGeneration && observation.authorizationFresh
  | .localSelf =>
      observation.signerMatchesRequester &&
      observation.requesterMatchesTarget
  | .runtimeInternal =>
      observation.runtimeEvidencePresent &&
      observation.signerMatchesIssuer && observation.signerMatchesTarget &&
      observation.targetRuntimeAttestationValid && observation.sourceBindingCurrent &&
      match observation.runtimeSourceKind with
      | .localChild =>
          observation.requesterMatchesIssuer && observation.requesterMatchesTarget &&
          observation.sourceDocumentBindingCurrent &&
          observation.sourceToolCallBindingCurrent && observation.targetPolicyAllows
      | .crossPrincipalChild =>
          observation.requesterMatchesBridgeAuthor &&
          observation.sourceToolCallBindingCurrent &&
          observation.bridgeAuthorBindingCurrent &&
          observation.bridgeAuthorAuthorizationFresh &&
          observation.targetCrossPrincipalPolicyAllows
      | .localControl =>
          observation.requesterMatchesIssuer && observation.requesterMatchesTarget &&
          observation.sourceDocumentBindingCurrent
      | .automatedTrigger =>
          observation.requesterMatchesIssuer && observation.requesterMatchesTarget &&
          observation.triggerConfigDocumentBindingCurrent && observation.targetPolicyAllows

/--
The claim boundary distinguishes an authoritative negative observation from an
unavailable observation.  Only the former may terminalize a durable request;
transport/store/identity unavailability leaves it pending for a later attempt.
-/
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

/-- The purpose-aware pending scan retries unavailable observations and routes
an authoritative title decision through the existing request transition owner.
An admit leaves this request Pending for Handover.claimTitle's canonical lease
claim; the separate output owner selects NoMessage for a rejected title. -/
def titlePendingDisposition
    (observationAvailable : Bool) (s : State) (request : AgentRequestSemantics)
    (admission : AgentRequestAdmission) (runtimeEvidence : Option RuntimeInternalEvidence)
    (sessionBehavior : String) (branchFieldsExact pendingDeadlineAbsent : Bool) :
    AgentRequestAdmissionDisposition :=
  if request.purpose != .titleAudit then .deny
  else admissionDispositionFromResult observationAvailable <|
    decide (agentRequestClaimable s request admission none none false runtimeEvidence
      branchFieldsExact pendingDeadlineAbsent sessionBehavior [] (fun _ => false) (fun _ => false))

def titlePendingStep?
    (observationAvailable : Bool) (s : State) (request : AgentRequestSemantics)
    (admission : AgentRequestAdmission) (runtimeEvidence : Option RuntimeInternalEvidence)
    (sessionBehavior : String) (branchFieldsExact pendingDeadlineAbsent : Bool)
    (pending : RequestContext) : Option RequestContext :=
  if request.purpose != .titleAudit || pending.state != .pending ||
      pending.admission != .released then none
  else
    match titlePendingDisposition observationAvailable s request admission runtimeEvidence
        sessionBehavior branchFieldsExact pendingDeadlineAbsent with
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
    (evidence : Option RuntimeInternalEvidence) (behavior : String)
    (branchFieldsExact pendingDeadlineAbsent : Bool)
    (pending : RequestContext)
    (hpurpose : request.purpose = .titleAudit)
    (hstate : pending.state = .pending) (hslot : pending.admission = .released) :
    titlePendingStep? false s request admission evidence behavior branchFieldsExact
      pendingDeadlineAbsent pending = some pending := by
  simp [titlePendingStep?, titlePendingDisposition, admissionDispositionFromResult,
    hpurpose, hstate, hslot]

theorem admitted_title_pending_awaits_owned_claim
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (evidence : Option RuntimeInternalEvidence) (behavior : String)
    (branchFieldsExact pendingDeadlineAbsent : Bool)
    (pending : RequestContext)
    (hpurpose : request.purpose = .titleAudit)
    (hstate : pending.state = .pending) (hslot : pending.admission = .released)
    (hadmit : titlePendingDisposition true s request admission evidence behavior
      branchFieldsExact pendingDeadlineAbsent = .admit) :
    titlePendingStep? true s request admission evidence behavior branchFieldsExact
      pendingDeadlineAbsent pending = some pending := by
  simp [titlePendingStep?, hpurpose, hstate, hslot, hadmit]

theorem denied_title_pending_uses_admission_reject
    (s : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (evidence : Option RuntimeInternalEvidence) (behavior : String)
    (branchFieldsExact pendingDeadlineAbsent : Bool)
    (pending : RequestContext)
    (hpurpose : request.purpose = .titleAudit)
    (hstate : pending.state = .pending) (hslot : pending.admission = .released)
    (hdeny : titlePendingDisposition true s request admission evidence behavior
      branchFieldsExact pendingDeadlineAbsent = .deny) :
    titlePendingStep? true s request admission evidence behavior branchFieldsExact
      pendingDeadlineAbsent pending =
      pending.step? .admissionReject := by
  simp [titlePendingStep?, hpurpose, hstate, hslot, hdeny]

theorem enrollment_requires_current_exact_generation
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool}
    (hkind : admission.kind = .enrollment)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent) :
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
    (branchFieldsExact pendingDeadlineAbsent : Bool)
    (hkind : admission.kind = .enrollment) :
    ¬ agentRequestAdmissible s request admission enrollmentRequest decision false runtimeEvidence
      branchFieldsExact pendingDeadlineAbsent := by
  rcases enrollmentRequest with _ | enrolledRequest <;> rcases decision with _ | approval <;>
    simp [agentRequestAdmissible, hkind]

theorem local_self_requires_exact_principal
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool}
    (hkind : admission.kind = .localSelf)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent) :
    admission.signerDid = request.requesterDid ∧ request.requesterDid = request.targetAgent := by
  simp only [agentRequestAdmissible, hkind] at hadmit
  rcases hadmit with ⟨_, _, _, _, ⟨hsigner, htarget⟩, _⟩
  exact ⟨hsigner, htarget⟩

/-- Enrollment history cannot disable a cryptographically exact local owner. -/
theorem local_self_admission_is_independent_of_enrollment_state
    (s₁ s₂ : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision) (fresh : Bool)
    (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool)
    (hkind : admission.kind = .localSelf) :
    agentRequestAdmissible s₁ request admission enrollmentRequest decision fresh runtimeEvidence
        branchFieldsExact pendingDeadlineAbsent ↔
      agentRequestAdmissible s₂ request admission enrollmentRequest decision fresh runtimeEvidence
        branchFieldsExact pendingDeadlineAbsent := by
  simp [agentRequestAdmissible, hkind]

theorem runtime_internal_requires_owned_issue
    {s : State} {request : AgentRequestSemantics} {admission : AgentRequestAdmission}
    {enrollmentRequest : Option Request} {decision : Option Decision} {fresh : Bool}
    {runtimeEvidence : Option RuntimeInternalEvidence}
    {branchFieldsExact pendingDeadlineAbsent : Bool}
    (hkind : admission.kind = .runtimeInternal)
    (hadmit : agentRequestAdmissible s request admission enrollmentRequest decision fresh
      runtimeEvidence branchFieldsExact pendingDeadlineAbsent) :
    ∃ evidence, runtimeEvidence = some evidence ∧
      exactRuntimeInternalEvidence request admission evidence := by
  simp only [agentRequestAdmissible, hkind] at hadmit
  rcases runtimeEvidence with _ | evidence <;> simp_all

/-- Target-runtime attestation, not a principal's enrollment history, owns internal admission. -/
theorem runtime_internal_admission_is_independent_of_enrollment_state
    (s₁ s₂ : State) (request : AgentRequestSemantics) (admission : AgentRequestAdmission)
    (enrollmentRequest : Option Request) (decision : Option Decision) (fresh : Bool)
    (runtimeEvidence : Option RuntimeInternalEvidence)
    (branchFieldsExact pendingDeadlineAbsent : Bool)
    (hkind : admission.kind = .runtimeInternal) :
    agentRequestAdmissible s₁ request admission enrollmentRequest decision fresh runtimeEvidence
        branchFieldsExact pendingDeadlineAbsent ↔
      agentRequestAdmissible s₂ request admission enrollmentRequest decision fresh runtimeEvidence
        branchFieldsExact pendingDeadlineAbsent := by
  simp [agentRequestAdmissible, hkind]

end Enrollment
