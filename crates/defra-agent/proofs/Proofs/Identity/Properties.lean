import Proofs.Identity.State
import Proofs.Identity.Permission

/-!
# Identity — Properties

I1–I5: the load-bearing theorems for the AgentPrincipal /
AgentBehavior / AgentDeployment boundary.
-/

namespace Identity

/-- **I1 Sharing.** Any decide that respects the principal boundary
    gives the same answer for two behaviors with the same principal. -/
theorem sharing
    {Permission : Type} (decide : Decide Permission)
    (h : RespectsPrincipal decide)
    (b₁ b₂ : Behavior) (p : Permission)
    (heq : b₁.principal = b₂.principal) :
    decide b₁ p = decide b₂ p :=
  h b₁ b₂ p heq

/-- **I2 Isolation** (contrapositive of I1). If two behaviors get
    different permission outcomes, they have different principals. -/
theorem isolation
    {Permission : Type} (decide : Decide Permission)
    (h : RespectsPrincipal decide)
    (b₁ b₂ : Behavior) (p : Permission)
    (hneq : decide b₁ p ≠ decide b₂ p) :
    b₁.principal ≠ b₂.principal := by
  intro heq
  exact hneq (h b₁ b₂ p heq)

end Identity
