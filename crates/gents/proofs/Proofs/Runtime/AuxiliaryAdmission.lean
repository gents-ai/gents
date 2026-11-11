import Proofs.Enrollment.RequestAdmission
import Proofs.CanonicalOutput.Execution.SessionComposition

namespace AuxiliaryAdmission

open CanonicalOutput.Execution

/-- Adapter must preserve distinct identities; a bare hash without collision checks does not. -/
structure Identities where
  encode : String → Nat
  injective : Function.Injective encode

theorem Identities.exact_equality (ids : Identities) (left right : String) :
    ids.encode left = ids.encode right ↔ left = right := ids.injective.eq_iff

/-- Authoritative observation of the auxiliary request's own physical row, not a requested document label. -/
structure RequestRowEvidence where
  physicalRequest : String
  logicalRequest : String
  signedFields : Enrollment.CanonicalFields
  physicalBindingCurrent : Bool
  branchFieldsExact : Bool
  pendingDeadlineAbsent : Bool
  maxRequestHop : Option Nat

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
    (agent : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) : Option Handover.AuxiliaryActivation :=
  if request.purpose.isAuxiliary = true ∧ rowBound row request admission ∧
      Enrollment.auxiliaryPendingDisposition available enrollment request admission evidence
        agent row.branchFieldsExact row.pendingDeadlineAbsent row.maxRequestHop = .admit then
    match evidence.bind (·.auxiliaryParent) with
    | none => none
    | some parent => some
        { binding :=
            { purpose := request.purpose
            , physicalRequest := ids.encode row.physicalRequest
            , logicalRequest := ids.encode request.requestId
            , parentPhysical := ids.encode parent.link.documentId
            , parentLogical := ids.encode parent.link.requestId
            , node := ids.encode request.targetNode
            , session := ids.encode request.sessionId
            , authenticated := true }
        , generation, duration, deadline }
  else none

theorem activation_requires_admitted_observation
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (agent : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (activation : Handover.AuxiliaryActivation)
    (h : activation? ids available enrollment request admission evidence agent
      row generation duration deadline = some activation) :
    request.purpose.isAuxiliary = true ∧ rowBound row request admission ∧
      Enrollment.auxiliaryPendingDisposition available enrollment request admission evidence
        agent row.branchFieldsExact row.pendingDeadlineAbsent row.maxRequestHop = .admit := by
  unfold activation? at h
  split at h
  · assumption
  · contradiction

theorem activation_binds_own_physical_and_logical_request
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (agent : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (activation : Handover.AuxiliaryActivation)
    (h : activation? ids available enrollment request admission evidence agent
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
    (agent : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (h : ¬ rowBound row request admission) :
    activation? ids available enrollment request admission evidence agent
      row generation duration deadline = none := by
  simp [activation?, h]

/-- Authenticated parent binding must precede claiming the auxiliary request's own lease. -/
def activate? (world : World) (actor : Gate.Actor) (now : Time)
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (agent : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (scope : Nat) (budget : CompletionRetry.Budget)
    (retryDeadline : Option Time) : Option World := do
  let activation ← activation? ids available enrollment request admission evidence
    agent row generation duration deadline
  SessionComposition.activateAuxiliary world actor now activation scope budget retryDeadline

theorem successful_activation_is_existing_claim
    (before after : World) (actor : Gate.Actor) (now : Time)
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (agent : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (scope : Nat) (budget : CompletionRetry.Budget)
    (retryDeadline : Option Time)
    (h : activate? before actor now ids available enrollment request admission evidence
      agent row generation duration deadline scope budget retryDeadline = some after) :
    ∃ activation,
      activation? ids available enrollment request admission evidence agent
        row generation duration deadline = some activation ∧
      SessionComposition.activateAuxiliary before actor now activation scope budget retryDeadline =
        some after := by
  unfold activate? at h
  cases ha : activation? ids available enrollment request admission evidence agent
      row generation duration deadline with
  | none => simp [ha] at h
  | some activation => exact ⟨activation, by simp [ha], by simpa [ha] using h⟩

theorem successful_activation_has_application_trace
    (before after : World) (actor : Gate.Actor) (now : Time)
    (ids : Identities) (available : Bool) (enrollment : Enrollment.State)
    (request : Enrollment.AgentRequestSemantics)
    (admission : Enrollment.AgentRequestAdmission)
    (evidence : Option Enrollment.RuntimeInternalEvidence)
    (agent : String) (row : RequestRowEvidence) (generation : Generation)
    (duration deadline : Time) (scope : Nat) (budget : CompletionRetry.Budget)
    (retryDeadline : Option Time)
    (h : activate? before actor now ids available enrollment request admission evidence
      agent row generation duration deadline scope budget retryDeadline = some after) :
    SessionComposition.Trace before after := by
  obtain ⟨activation, _, hclaim⟩ := successful_activation_is_existing_claim before after
    actor now ids available enrollment request admission evidence agent row
    generation duration deadline scope budget retryDeadline h
  exact .activateAuxiliary actor now activation scope budget retryDeadline hclaim

end AuxiliaryAdmission
