import Proofs.Identity.State
import Proofs.Identity.Permission

namespace Identity

theorem sharing
    {Permission : Type} (decide : Decide Permission)
    (h : RespectsNode decide)
    (b₁ b₂ : Agent) (p : Permission)
    (heq : b₁.node = b₂.node) :
    decide b₁ p = decide b₂ p :=
  h b₁ b₂ p heq

theorem isolation
    {Permission : Type} (decide : Decide Permission)
    (h : RespectsNode decide)
    (b₁ b₂ : Agent) (p : Permission)
    (hneq : decide b₁ p ≠ decide b₂ p) :
    b₁.node ≠ b₂.node := by
  intro heq
  exact hneq (h b₁ b₂ p heq)

theorem no_escalation
    {Permission : Type} (g : GrantStore Permission)
    (b : Agent) (p : Permission) :
    canonicalDecide g b p = g.granted b.node p := rfl

/-- Node and label together identify one agent; a shared label alone
never determines a node or a permission decision. -/
theorem scoped_agent_key_unique
    (w : World) (hw : w.WellFormed) (b₁ b₂ : Agent)
    (h₁ : b₁ ∈ w.agents) (h₂ : b₂ ∈ w.agents)
    (howner : b₁.node = b₂.node) (hid : b₁.id = b₂.id) : b₁ = b₂ :=
  hw.2.1 b₁ h₁ b₂ h₂ howner hid

theorem selected_agent_has_exact_scope
    (agents : List Agent) (node : DID) (id : AgentId) (selected : Agent)
    (h : findAgent? agents node id = some selected) :
    selected ∈ agents ∧ selected.node = node ∧ selected.id = id := by
  unfold findAgent? at h
  split at h
  · rename_i agent heq
    simp only [Option.some.injEq] at h
    subst selected
    have hm : agent ∈ agents.filter (fun b => b.node == node && b.id == id) := by
      rw [heq]; simp
    simpa using hm
  · contradiction

end Identity
