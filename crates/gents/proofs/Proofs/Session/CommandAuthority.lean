import Proofs.Basic

namespace AgentSession.CommandAuthority

/-- Facts supplied by the existing DID signature, canonical session,
enrollment and applied pairing-route owners. A local command has no peer
route and must be authored by the node itself. -/
structure Facts where
  signatureValid : Bool
  sessionOwned : Bool
  requesterIsNode : Bool
  enrollmentFresh : Bool
  routeApplied : Bool
  deriving DecidableEq, Repr

def authorized (facts : Facts) (hasPeer : Bool) (now expiresAt : Nat) : Bool :=
  facts.signatureValid && facts.sessionOwned && decide (now < expiresAt) &&
    if hasPeer then facts.enrollmentFresh && facts.routeApplied else facts.requesterIsNode

theorem invalid_signature_is_denied (facts : Facts) (peer : Bool) (now expires : Nat)
    (h : facts.signatureValid = false) : authorized facts peer now expires = false := by
  simp [authorized, h]

theorem remote_requires_current_enrollment (facts : Facts) (now expires : Nat)
    (h : facts.enrollmentFresh = false) : authorized facts true now expires = false := by
  simp [authorized, h]

theorem local_requires_node_author (facts : Facts) (now expires : Nat)
    (h : facts.requesterIsNode = false) : authorized facts false now expires = false := by
  simp [authorized, h]

end AgentSession.CommandAuthority
