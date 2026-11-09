/-!
Pairing persistence after one route reconcile (`combined_route_readiness`).
`pairing_ready` is a P2P route observation of an enrollment peer (a joined
remote node) and never gates chat writes (`ClientChatReadiness`). The local
node is never paired and carries no pairing readiness: its readiness is the
User's private status read (`Identity.NodeProvisioning.localStartup`).
-/

namespace ClientRoutePairing

/-- One reconcile leg's outcome. -/
inductive Leg where
  | ready
  | drifted
  | unavailable
  deriving DecidableEq, Repr

/-- Confirmed drift closes, both legs ready opens, and an unavailable
observation keeps the prior value until a retry observes. -/
def combinedLegs : Leg → Leg → Option Bool
  | .ready, .ready => some true
  | .drifted, _ => some false
  | _, .drifted => some false
  | _, _ => none

theorem unavailable_preserves_prior_readiness (other : Leg) (h : other ≠ .drifted) :
    combinedLegs .unavailable other = none ∧ combinedLegs other .unavailable = none := by
  cases other <;> simp_all [combinedLegs]

def allLegs : List Leg := [.ready, .drifted, .unavailable]

end ClientRoutePairing
