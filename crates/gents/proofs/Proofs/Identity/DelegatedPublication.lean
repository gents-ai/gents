import Proofs.Identity.SessionChild
import Proofs.Request.CrossActorAuthorization
import Proofs.Enrollment.RequestAdmission
import Lean

namespace Identity.DelegatedPublication

open Identity.Household Request.CrossActorAuthorization Lean

/-- This is the new local child case. The immediate parent Agent is the native
creator and signer. The physical session and Agent identifiers must occur in
the signed request; provenance verification is performed before committing the
transaction, after the staged opening request exists. -/
structure Binding where
  nodeOwner : String
  parentAgent : String
  targetAgent : String
  sessionDoc : String
  agentDoc : String
  requestDoc : String
  signedSession : String
  signedAgent : String
  signer : String
  deriving Repr

structure Links where
  sessionParent : Bool := false
  sessionWorker : Bool := false
  requestParent : Bool := false
  requestSession : Bool := false
  requestWorker : Bool := false
  deriving Repr

structure Publication where
  household : Identity.Household.State
  binding : Binding
  attempted : Bool := false
  committed : Bool := false
  observed : Bool := false
  toolRunning : Bool := false
  receipt : Bool := false
  starterVerified : Bool := false
  creationReceipt : Bool := false
  sessionPending : Bool := false
  requestPending : Bool := false
  links : Links := {}

def intent (b : Binding) : Intent :=
  ⟨b.requestDoc, b.sessionDoc, b.agentDoc, b.parentAgent, b.targetAgent⟩

def signedBinding (b : Binding) : Bool :=
  b.signer == b.parentAgent && signedTargetsMatch b.signedSession b.signedAgent (intent b)

/-- The external native transaction is the only premise for atomic tool start,
opening request, session, immutable verified starter and receipt. A failed
transaction leaves the publication unchanged. A lost acknowledgement is not
failure; it is represented by `committed` with `observed = false`. -/
def commit (s : Publication) (starterVerified creationReceipt transactionCommitted : Bool) : Publication :=
  if !s.attempted && signedBinding s.binding && starterVerified && creationReceipt && transactionCommitted &&
      !(s.household.objects.any (fun o => o.resource == "sessions" && o.id == s.binding.sessionDoc)) &&
      !(s.household.objects.any (fun o => o.resource == "delivered_requests" && o.id == s.binding.requestDoc)) then
    let withSession := directCreate s.household s.binding.parentAgent "sessions" s.binding.sessionDoc true true
    let withRequest := directCreate withSession s.binding.parentAgent "delivered_requests" s.binding.requestDoc true true
    { s with
      household := withRequest
      attempted := true
      committed := true
      toolRunning := true
      receipt := true
      starterVerified := true
      creationReceipt := true
      sessionPending := true
      requestPending := true }
  else { s with attempted := true }

/-- A retained receipt is evidence only when it authenticates the exact
binding and native creator. Failure to observe one does not authorize replay. -/
def observe (s : Publication) (exactReceipt matchingOwner : Bool) : Publication :=
  if s.committed && s.receipt && exactReceipt && matchingOwner && signedBinding s.binding then
    { s with observed := true }
  else s

structure GrantAttempt where
  sessionParent : Bool
  sessionWorker : Bool
  requestParent : Bool
  requestSession : Bool
  requestWorker : Bool
  markSession : Bool
  markRequest : Bool
  deriving Repr

def addIf (state : Identity.Household.State) (actor : String) (ok : Bool)
    (relation : Relation) : Identity.Household.State :=
  if ok then Identity.Household.apply state (.grantAs actor relation) else state

/-- Each relation is detached from the atomic document transaction. Only the
authenticated creator recovering the exact confirmed publication may finish a
pending set. A cleared marker is never replayed after later revocation. -/
def publish (s : Publication) (actor : String) (exactBinding : Bool) (g : GrantAttempt) : Publication :=
  if !s.observed || actor != s.binding.parentAgent || !exactBinding then s else
    let b := s.binding
    let sp := s.sessionPending && g.sessionParent
    let sw := s.sessionPending && g.sessionWorker
    let rp := s.requestPending && g.requestParent
    let rs := s.requestPending && g.requestSession
    let rw := s.requestPending && g.requestWorker
    let h1 := addIf s.household actor sp ⟨"sessions", b.sessionDoc, "parent", .object "agents" b.agentDoc⟩
    let h2 := addIf h1 actor sw ⟨"sessions", b.sessionDoc, "working_agent", .actor b.targetAgent⟩
    let h3 := addIf h2 actor rp ⟨"delivered_requests", b.requestDoc, "parent", .object "agents" b.agentDoc⟩
    let h4 := addIf h3 actor rs ⟨"delivered_requests", b.requestDoc, "session", .object "sessions" b.sessionDoc⟩
    let h5 := addIf h4 actor rw ⟨"delivered_requests", b.requestDoc, "working_agent", .actor b.targetAgent⟩
    let links : Links := ⟨s.links.sessionParent || sp, s.links.sessionWorker || sw,
      s.links.requestParent || rp, s.links.requestSession || rs, s.links.requestWorker || rw⟩
    { s with
      household := h5
      links := links
      sessionPending := s.sessionPending && !(links.sessionParent && links.sessionWorker && g.markSession)
      requestPending := s.requestPending && !(links.requestParent && links.requestSession && links.requestWorker && g.markRequest) }

/-- Native claim reads both pending markers in the same transaction that
checks the immediate author's session update and target Agent invoke. The
`NativeCommit.committed` premise includes native serialization of those marker
reads: a conflicting change cannot count as a committed claim. Markers gate
readiness; they do not prove that a relation remains after revocation. -/
def claim (s : Publication) (commit : NativeCommit) : Decision :=
  if !s.observed || s.sessionPending || s.requestPending then .unavailable else
  let b := s.binding
  let readable := allowed 5 s.household b.targetAgent "delivered_requests" b.requestDoc "read"
  let sessionPermission := allowed 5 s.household b.parentAgent "sessions" b.sessionDoc "update"
  let agentInvoke := allowed 5 s.household b.parentAgent "agents" b.agentDoc "invoke"
  if b.parentAgent == b.targetAgent then
    let sameActor : Enrollment.AgentRequestAdmissionObservation := {
      kind := .client, signatureValid := signedBinding b, signedFieldsMatch := true,
      branchFieldsExact := true, pendingDeadlineAbsent := true,
      signerMatchesRequester := true, requesterMatchesTarget := true,
      signerMatchesTarget := true, signerMatchesIssuer := false,
      requesterMatchesSessionScope := true, currentApproval := false,
      exactGeneration := false, authorizationFresh := false, runtimeEvidencePresent := false,
      runtimeSourceKind := .localControl, targetRuntimeAttestationValid := false,
      sourceBindingCurrent := false, triggerConfigDocumentBindingCurrent := false,
      sourceDocumentBindingCurrent := false, targetPolicyAllows := false,
      nativeCreateAuthorized := true, hopWithinBound := true }
    if !readable then .unavailable else if !commit.committed then .conflict else
      if Enrollment.projectAgentRequestAdmission sameActor then .claimed else .denied
  else
    let observation : Observation :=
      ⟨true, readable, true, false, b.requestDoc, b.sessionDoc, b.agentDoc,
        some b.parentAgent, some b.parentAgent, b.targetAgent,
        signedBinding b, sessionPermission, agentInvoke, true, true⟩
    decideClaim (intent b) observation commit

theorem unobserved_cannot_publish (s : Publication) (actor : String) (g : GrantAttempt)
    (h : s.observed = false) : publish s actor true g = s := by
  simp [publish, h]

theorem pending_request_cannot_claim (s : Publication) (c : NativeCommit)
    (h : s.requestPending = true) : claim s c = .unavailable := by
  simp [claim, h]

end Identity.DelegatedPublication
