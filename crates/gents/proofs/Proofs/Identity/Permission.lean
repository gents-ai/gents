import Proofs.Identity.State

namespace Identity

structure GrantStore (Permission : Type) where
  granted : DID → Permission → Bool

abbrev Decide (Permission : Type) := Agent → Permission → Bool

def RespectsNode {Permission : Type} (decide : Decide Permission) : Prop :=
  ∀ (b₁ b₂ : Agent) (p : Permission),
    b₁.node = b₂.node → decide b₁ p = decide b₂ p

def canonicalDecide {Permission : Type} (g : GrantStore Permission) :
    Decide Permission :=
  fun b p => g.granted b.node p

theorem canonicalDecide_respectsNode
    {Permission : Type} (g : GrantStore Permission) :
    RespectsNode (canonicalDecide g) := by
  intro b₁ b₂ p heq
  unfold canonicalDecide
  rw [heq]

end Identity
