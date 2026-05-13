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

/-- **I3 No-escalation** (under the canonical construction).
    A behavior's effective decision is entirely determined by its
    principal's grants; no field of `Behavior` can widen access. -/
theorem no_escalation
    {Permission : Type} (g : GrantStore Permission)
    (b : Behavior) (p : Permission) :
    canonicalDecide g b p = g.granted b.principal p := rfl

/-- **I4 Behavior-id functionally determines principal.** In any
    well-formed world, `Behavior.id` is unique and therefore
    `behavior_id → principal` is a function. Closes the
    "`(did, behavior_id)` uniqueness" criterion. -/
theorem behavior_id_determines_principal
    (w : World) (hw : w.WellFormed)
    (b₁ b₂ : Behavior)
    (h₁ : b₁ ∈ w.behaviors) (h₂ : b₂ ∈ w.behaviors)
    (hid : b₁.id = b₂.id) :
    b₁.principal = b₂.principal := by
  have hbeh := hw.2.1
  have heq : b₁ = b₂ := hbeh b₁ b₂ h₁ h₂ hid
  rw [heq]

end Identity
