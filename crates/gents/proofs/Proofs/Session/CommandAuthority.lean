import Proofs.Basic

namespace AgentSession.CommandAuthority

/-- Facts supplied by the DID signature and canonical session owners. A direct
client signs as itself; session ownership here means exact target binding, not
actor equality with creation requester. Native document ACP authorizes writes
(policy relationships follow in #3485). Enrollment and route evidence apply only
to actual peer delivery; direct document operations do not require a Node key. -/
structure Facts where
  signatureValid : Bool
  sessionOwned : Bool
  requesterIsNode : Bool
  enrollmentFresh : Bool
  routeApplied : Bool
  deriving DecidableEq, Repr

def authorized (facts : Facts) (hasPeer : Bool) (now expiresAt : Nat) : Bool :=
  facts.signatureValid && facts.sessionOwned && decide (now < expiresAt) &&
    if hasPeer then facts.enrollmentFresh && facts.routeApplied else true

theorem invalid_signature_is_denied (facts : Facts) (peer : Bool) (now expires : Nat)
    (h : facts.signatureValid = false) : authorized facts peer now expires = false := by
  simp [authorized, h]

theorem remote_requires_current_enrollment (facts : Facts) (now expires : Nat)
    (h : facts.enrollmentFresh = false) : authorized facts true now expires = false := by
  simp [authorized, h]

theorem direct_command_independent_of_node_key (facts : Facts) (now expires : Nat) :
    authorized { facts with requesterIsNode := false } false now expires =
      authorized { facts with requesterIsNode := true } false now expires := by
  simp [authorized]

end AgentSession.CommandAuthority
