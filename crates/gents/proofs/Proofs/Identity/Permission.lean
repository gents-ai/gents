import Proofs.Identity.State

namespace Identity

structure GrantStore (Permission : Type) where
  granted : DID → Permission → Bool

abbrev Decide (Permission : Type) := Behavior → Permission → Bool

def RespectsPrincipal {Permission : Type} (decide : Decide Permission) : Prop :=
  ∀ (b₁ b₂ : Behavior) (p : Permission),
    b₁.principal = b₂.principal → decide b₁ p = decide b₂ p

def canonicalDecide {Permission : Type} (g : GrantStore Permission) :
    Decide Permission :=
  fun b p => g.granted b.principal p

theorem canonicalDecide_respectsPrincipal
    {Permission : Type} (g : GrantStore Permission) :
    RespectsPrincipal (canonicalDecide g) := by
  intro b₁ b₂ p heq
  unfold canonicalDecide
  rw [heq]

end Identity
