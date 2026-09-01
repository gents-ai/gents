import Proofs.Enrollment.Transition

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
  | crossDeploymentChild
  | localControl
  | automatedTrigger
  deriving DecidableEq, Repr

/-- Exact immutable request semantics covered by the request signature. -/
structure AgentRequestSemantics where
  requestId : String
  targetAgent : Did
  requesterDid : Did
  behaviorId : String
  sessionId : String
  content : String
  samplingFields : CanonicalFields
  metadata : String
  createdAt : String
  /-- Physical `_docID` of the Schedule/EventTrigger configuration row. -/
  triggerConfigDocumentId : String
  retryFields : CanonicalFields
  triggerFields : CanonicalFields
  parentFields : CanonicalFields
  workspaceFields : CanonicalFields
  deriving DecidableEq, Repr

def agentRequestSemanticFields (request : AgentRequestSemantics) : CanonicalFields :=
  textFieldsToBytes
    [ request.requestId, request.targetAgent, request.requesterDid
    , request.behaviorId, request.sessionId, request.content ] ++
  request.samplingFields ++ textFieldsToBytes
    [request.metadata, request.createdAt, request.triggerConfigDocumentId] ++
  request.retryFields ++ request.triggerFields ++ request.parentFields ++ request.workspaceFields

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
          | .crossDeploymentChild => "cross-deployment-child"
          | .localControl => "local-control"
          | .automatedTrigger => "automated-trigger"
      | _ => ""
    , admission.bridgeAuthorDid ]

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
  sourceBindingCurrent : Bool
  triggerConfigDocumentBindingCurrent : Bool
  sourceDocumentBindingCurrent : Bool
  sourceToolCallBindingCurrent : Bool
  targetPolicyAllows : Bool
  bridgeAuthorBindingCurrent : Bool
  bridgeAuthorAuthorizationFresh : Bool
  targetCrossDeploymentPolicyAllows : Bool
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
  request.requesterDid = request.targetAgent ∧
  evidence.targetRuntimeAttestationValid = true ∧
  evidence.sourceBindingCurrent = true ∧
  match admission.runtimeSourceKind with
  | .localChild =>
      admission.bridgeAuthorDid = "" ∧ evidence.bridgeAuthorDid = "" ∧
      evidence.sourceDocumentBindingCurrent = true ∧
      evidence.sourceToolCallBindingCurrent = true ∧
      evidence.targetPolicyAllows = true
  | .crossDeploymentChild =>
      admission.bridgeAuthorDid ≠ "" ∧
      evidence.bridgeAuthorDid = admission.bridgeAuthorDid ∧
      evidence.sourceToolCallBindingCurrent = true ∧
      evidence.bridgeAuthorBindingCurrent = true ∧
      evidence.bridgeAuthorAuthorizationFresh = true ∧
      evidence.targetCrossDeploymentPolicyAllows = true
  | .localControl =>
      admission.bridgeAuthorDid = "" ∧ evidence.bridgeAuthorDid = "" ∧
      evidence.sourceDocumentBindingCurrent = true
  | .automatedTrigger =>
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
  match admission.kind with
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
      | none => False

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
  targetCrossDeploymentPolicyAllows : Bool
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
      observation.signerMatchesIssuer && observation.requesterMatchesIssuer &&
      observation.signerMatchesTarget && observation.requesterMatchesTarget &&
      observation.targetRuntimeAttestationValid && observation.sourceBindingCurrent &&
      match observation.runtimeSourceKind with
      | .localChild =>
          observation.sourceDocumentBindingCurrent &&
          observation.sourceToolCallBindingCurrent && observation.targetPolicyAllows
      | .crossDeploymentChild =>
          observation.sourceToolCallBindingCurrent &&
          observation.bridgeAuthorBindingCurrent &&
          observation.bridgeAuthorAuthorizationFresh &&
          observation.targetCrossDeploymentPolicyAllows
      | .localControl => observation.sourceDocumentBindingCurrent
      | .automatedTrigger =>
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

def projectAgentRequestAdmissionDisposition
    (observationAvailable : Bool) (observation : AgentRequestAdmissionObservation) :
    AgentRequestAdmissionDisposition :=
  if !observationAvailable then .retry
  else if projectAgentRequestAdmission observation then .admit
  else .deny

theorem unavailable_admission_observation_retries
    (observation : AgentRequestAdmissionObservation) :
    projectAgentRequestAdmissionDisposition false observation = .retry := by
  simp [projectAgentRequestAdmissionDisposition]

theorem available_negative_admission_observation_denies
    (observation : AgentRequestAdmissionObservation)
    (hdeny : projectAgentRequestAdmission observation = false) :
    projectAgentRequestAdmissionDisposition true observation = .deny := by
  simp [projectAgentRequestAdmissionDisposition, hdeny]

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
  rcases hadmit with ⟨_, _, _, _, hsigner, htarget⟩
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
