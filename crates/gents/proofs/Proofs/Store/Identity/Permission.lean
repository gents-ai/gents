import Proofs.Identity.State

namespace Identity

/-- DefraDB owns authorization. Product write/admin capabilities must be expressed as
native document read/update/delete permissions and ownership, never an additional
Gents permission decision. Policy wiring is follow-up work; this model records the
required selected actor and does not claim that every runtime path enforces it. -/
structure GrantStore (Permission : Type) where
  granted : DID → Permission → Bool

abbrev Decide (Permission : Type) := Agent → Permission → Bool

def RespectsActor {Permission : Type} (decide : Decide Permission) : Prop :=
  ∀ (b₁ b₂ : Agent) (p : Permission),
    b₁.did = b₂.did → decide b₁ p = decide b₂ p

def canonicalDecide {Permission : Type} (g : GrantStore Permission) :
    Decide Permission :=
  fun b p => g.granted b.did p

theorem canonicalDecide_respectsActor
    {Permission : Type} (g : GrantStore Permission) :
    RespectsActor (canonicalDecide g) := by
  intro b₁ b₂ p heq
  unfold canonicalDecide
  rw [heq]

/-- The runtime resolves the agent once; an absent selection never inherits the node actor. -/
def selectedDecide {Permission : Type} (g : GrantStore Permission)
    (selected : Option Agent) (permission : Permission) : Bool :=
  selected.any (fun agent => canonicalDecide g agent permission)

theorem missing_selection_denied {Permission : Type} (g : GrantStore Permission)
    (permission : Permission) : selectedDecide g none permission = false := rfl

theorem denied_agent_never_falls_back {Permission : Type} (g : GrantStore Permission)
    (agent : Agent) (permission : Permission) (h : g.granted agent.did permission = false) :
    selectedDecide g (some agent) permission = false := h

end Identity
