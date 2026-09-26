import Proofs.Enrollment.RequestAdmission
import Proofs.CanonicalOutput.Execution.SessionComposition

namespace TitleAdmission

open CanonicalOutput.Execution

/-- Native document/DID strings are opaque Nat labels in the execution model.
The adapter must preserve distinct identities; a hash without collision checks
does not establish this interface premise. -/
structure Identities where
  encode : String → Nat
  injective : Function.Injective encode

theorem Identities.exact_equality (ids : Identities) (left right : String) :
    ids.encode left = ids.encode right ↔ left = right := ids.injective.eq_iff

/-- Authoritative observation of the title's own physical row, not a requested
document label. Native decoding establishes `physicalBindingCurrent` from that
row's identity and immutable signed contents before constructing this evidence. -/
structure RequestRowEvidence where
  physicalRequest : String
  logicalRequest : String
  signedFields : Enrollment.CanonicalFields
  physicalBindingCurrent : Bool
  branchFieldsExact : Bool
  pendingDeadlineAbsent : Bool
  /-- The target principal's configured `max_request_hop`, read by the same
  admission observation. -/
  maxRequestHop : Nat

def rowBound (row : RequestRowEvidence) (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission) : Prop :=
  row.physicalRequest ≠ "" ∧ row.logicalRequest = request.requestId ∧
    row.signedFields = Enrollment.agentRequestAdmissionFields request admission ∧
    row.physicalBindingCurrent = true

instance (row : RequestRowEvidence) (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission) : Decidable (rowBound row request admission) := by
  unfold rowBound
  infer_instance

def activation? (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (behavior : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) : Option Handover.TitleActivation :=
  if request.purpose = .titleAudit ∧ rowBound row request admission ∧
      Enrollment.titlePendingDisposition available enrollment request admission evidence
        behavior row.branchFieldsExact row.pendingDeadlineAbsent row.maxRequestHop = .admit then
    match evidence.bind (·.titleParent) with
    | none => none
    | some parent => some
        { binding :=
            { physicalRequest := ids.encode row.physicalRequest
            , logicalRequest := ids.encode request.requestId
            , parentPhysical := ids.encode parent.link.documentId
            , parentLogical := ids.encode parent.link.requestId
            , agent := ids.encode request.targetAgent
            , session := ids.encode request.sessionId
            , authenticated := true }
        , generation, duration, deadline }
  else none

theorem activation_requires_admitted_observation
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (behavior : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (activation : Handover.TitleActivation)
    (h : activation? ids available enrollment request admission evidence behavior
      row generation duration deadline = some activation) :
    request.purpose = .titleAudit ∧ rowBound row request admission ∧
      Enrollment.titlePendingDisposition available enrollment request admission evidence
        behavior row.branchFieldsExact row.pendingDeadlineAbsent row.maxRequestHop = .admit := by
  unfold activation? at h
  split at h
  · assumption
  · contradiction

theorem activation_binds_own_physical_and_logical_request
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (behavior : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (activation : Handover.TitleActivation)
    (h : activation? ids available enrollment request admission evidence behavior
      row generation duration deadline = some activation) :
    activation.binding.physicalRequest = ids.encode row.physicalRequest ∧
      activation.binding.logicalRequest = ids.encode request.requestId := by
  unfold activation? at h
  repeat' first | contradiction | split at h
  all_goals cases h
  all_goals exact ⟨rfl, rfl⟩

theorem unbound_row_cannot_activate
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (behavior : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (h : ¬ rowBound row request admission) :
    activation? ids available enrollment request admission evidence behavior
      row generation duration deadline = none := by
  simp [activation?, h]

/-- Admission authenticates the immutable parent binding before the existing
gate claims the title's own lease. An unavailable/denied observation cannot
manufacture authenticated execution evidence or consume a session queue entry. -/
def activate? (world : World) (actor : Gate.Actor) (now : Time)
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (behavior : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (scope : Nat) (budget : CompletionRetry.Budget)
    (retryDeadline : Option Time) : Option World := do
  let activation ← activation? ids available enrollment request admission evidence
    behavior row generation duration deadline
  SessionComposition.activateTitle world actor now activation scope budget retryDeadline

theorem successful_activation_is_existing_claim
    (before after : World) (actor : Gate.Actor) (now : Time)
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (behavior : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (scope : Nat) (budget : CompletionRetry.Budget)
    (retryDeadline : Option Time)
    (h : activate? before actor now ids available enrollment request admission evidence
      behavior row generation duration deadline scope budget retryDeadline = some after) :
    ∃ activation,
      activation? ids available enrollment request admission evidence behavior
        row generation duration deadline = some activation ∧
      SessionComposition.activateTitle before actor now activation scope budget retryDeadline =
        some after := by
  unfold activate? at h
  cases ha : activation? ids available enrollment request admission evidence behavior
      row generation duration deadline with
  | none => simp [ha] at h
  | some activation => exact ⟨activation, by simp [ha], by simpa [ha] using h⟩

theorem successful_activation_has_application_trace
    (before after : World) (actor : Gate.Actor) (now : Time)
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (behavior : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (scope : Nat) (budget : CompletionRetry.Budget)
    (retryDeadline : Option Time)
    (h : activate? before actor now ids available enrollment request admission evidence
      behavior row generation duration deadline scope budget retryDeadline = some after) :
    SessionComposition.Trace before after := by
  obtain ⟨activation, _, hclaim⟩ := successful_activation_is_existing_claim before after
    actor now ids available enrollment request admission evidence behavior row
    generation duration deadline scope budget retryDeadline h
  exact .activateTitle actor now activation scope budget retryDeadline hclaim

end TitleAdmission
