import Proofs.Enrollment.DurableProjection
import Proofs.Background.ProcessControl

namespace Enrollment

/-- DefraDB authenticates the HTTP actor. The existing enrollment owner supplies
only a unique, current authorization; revoked, expired, conflicted, and ambiguous
generations supply `none`. This read admits client metadata without exporting the
runtime's credential or its raw backend configuration. -/
def clientObservationAdmitted (actor : Option Did) (servingNode : Did)
    (current : Option RequestAuthority) : Bool :=
  match actor, current with
  | some member, some authority => decide (
      member ≠ "" ∧ servingNode ≠ "" ∧ authority.candidateDid = member ∧
      authority.ownerNode = servingNode ∧ authority.profile = "client")
  | _, _ => false

/-- Runtime activity is available to the authenticated node operator or to a
currently enrolled client. Document visibility is checked separately in the
same database transaction as the exact request selection. -/
def runtimeObservationAdmitted (actor : Option Did) (servingNode : Did)
    (current : Option RequestAuthority) : Bool :=
  (servingNode != "" && Background.ProcessControl.operatorAtNode
    servingNode servingNode servingNode actor) ||
  clientObservationAdmitted actor servingNode current


theorem unauthenticated_observation_denied (node : Did)
    (current : Option RequestAuthority) :
    clientObservationAdmitted none node current = false := by
  cases current <;> rfl

theorem unknown_observation_denied (actor : Option Did) (node : Did) :
    clientObservationAdmitted actor node none = false := by
  cases actor <;> rfl

theorem admitted_observation_requires_client_scope
    {member node : Did} {authority : RequestAuthority}
    (h : clientObservationAdmitted (some member) node (some authority) = true) :
    authority.candidateDid = member ∧ authority.ownerNode = node ∧
      authority.profile = "client" := by
  have facts : member ≠ "" ∧ node ≠ "" ∧ authority.candidateDid = member ∧
      authority.ownerNode = node ∧ authority.profile = "client" := by
    simpa only [clientObservationAdmitted, decide_eq_true_eq] using h
  exact facts.2.2

theorem other_node_observation_denied (member node : Did) (authority : RequestAuthority)
    (h : authority.ownerNode ≠ node) :
    clientObservationAdmitted (some member) node (some authority) = false := by
  simp [clientObservationAdmitted, h]

theorem other_profile_observation_denied (member node : Did) (authority : RequestAuthority)
    (h : authority.profile ≠ "client") :
    clientObservationAdmitted (some member) node (some authority) = false := by
  simp [clientObservationAdmitted, h]

end Enrollment
