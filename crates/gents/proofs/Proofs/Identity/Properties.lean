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

/-- Principal and label together identify one behavior; a shared label alone
never determines a principal or a permission decision. -/
theorem scoped_behavior_key_unique
    (w : World) (hw : w.WellFormed) (b₁ b₂ : Behavior)
    (h₁ : b₁ ∈ w.behaviors) (h₂ : b₂ ∈ w.behaviors)
    (howner : b₁.principal = b₂.principal) (hid : b₁.id = b₂.id) : b₁ = b₂ :=
  hw.2.1 b₁ h₁ b₂ h₂ howner hid

theorem selected_behavior_has_exact_scope
    (behaviors : List Behavior) (principal : DID) (id : BehaviorId) (selected : Behavior)
    (h : findBehavior? behaviors principal id = some selected) :
    selected ∈ behaviors ∧ selected.principal = principal ∧ selected.id = id := by
  unfold findBehavior? at h
  split at h
  · rename_i behavior heq
    simp only [Option.some.injEq] at h
    subst selected
    have hm : behavior ∈ behaviors.filter (fun b => b.principal == principal && b.id == id) := by
      rw [heq]; simp
    simpa using hm
  · contradiction

end Identity
