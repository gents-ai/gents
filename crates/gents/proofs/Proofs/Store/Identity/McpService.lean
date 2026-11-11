import Proofs.Identity.Household

namespace Identity.McpService

/-- Each network use observes the current physical Agent/Context/Tools chain
and service under the executing Agent. Visibility is supplied by native ACP;
the selected logical service and its endpoint come from those fresh rows.
The cached selection must still match the observed Tools configuration. A
revocation after network dispatch cannot recall an already sent request. -/
def mayUse (sameActor sameChain sameSelection selected serviceVisible enabled : Bool) : Bool :=
  sameActor && sameChain && sameSelection && selected && serviceVisible && enabled

theorem revoked_service_is_not_used (actor chain selection selected enabled : Bool) :
    mayUse actor chain selection selected false enabled = false := by
  simp [mayUse]

theorem rebound_selection_is_not_used (actor chain selected visible enabled : Bool) :
    mayUse actor chain false selected visible enabled = false := by
  simp [mayUse]

theorem wrong_physical_chain_is_not_used (actor selection selected visible enabled : Bool) :
    mayUse actor false selection selected visible enabled = false := by
  simp [mayUse]

end Identity.McpService
