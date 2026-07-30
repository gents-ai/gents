import Proofs.Identity.State
import Proofs.Identity.Permission

namespace Identity

theorem sharing
    {Permission : Type} (decide : Decide Permission)
    (h : RespectsPrincipal decide)
    (b₁ b₂ : Behavior) (p : Permission)
    (heq : b₁.principal = b₂.principal) :
    decide b₁ p = decide b₂ p :=
  h b₁ b₂ p heq

theorem isolation
    {Permission : Type} (decide : Decide Permission)
    (h : RespectsPrincipal decide)
    (b₁ b₂ : Behavior) (p : Permission)
    (hneq : decide b₁ p ≠ decide b₂ p) :
    b₁.principal ≠ b₂.principal := by
  intro heq
  exact hneq (h b₁ b₂ p heq)

theorem no_escalation
    {Permission : Type} (g : GrantStore Permission)
    (b : Behavior) (p : Permission) :
    canonicalDecide g b p = g.granted b.principal p := rfl

theorem behavior_id_determines_principal
    (w : World) (hw : w.WellFormed)
    (b₁ b₂ : Behavior)
    (h₁ : b₁ ∈ w.behaviors) (h₂ : b₂ ∈ w.behaviors)
    (hid : b₁.id = b₂.id) :
    b₁.principal = b₂.principal := by
  have hbeh := hw.2.1
  have heq : b₁ = b₂ := hbeh b₁ b₂ h₁ h₂ hid
  rw [heq]

def Deployment.canHostBehavior (d : Deployment) (b : Behavior) : Bool :=
  d.principal == b.principal

theorem co_hostable_share_principal
    (d : Deployment) (b₁ b₂ : Behavior)
    (h₁ : d.canHostBehavior b₁ = true)
    (h₂ : d.canHostBehavior b₂ = true) :
    b₁.principal = b₂.principal := by
  unfold Deployment.canHostBehavior at h₁ h₂
  have e₁ : d.principal = b₁.principal := by
    simpa [beq_iff_eq] using h₁
  have e₂ : d.principal = b₂.principal := by
    simpa [beq_iff_eq] using h₂
  exact e₁.symm.trans e₂

end Identity
