import Proofs.Identity.State

/-!
# Identity — Permission

Engine-agnostic permission decision interface. `Permission` is a
free type parameter — Cedar's `(action, resource)`, Zanzibar's
`(relation, object)`, or any future representation instantiates it
at the use site. The load-bearing predicate is `RespectsPrincipal`:
the decision must factor through `b.principal`. The slim `Behavior`
struct (see `State.lean`) enforces this by construction over
modeled fields.
-/

namespace Identity

structure GrantStore (Permission : Type) where
  granted : DID → Permission → Bool

abbrev Decide (Permission : Type) := Behavior → Permission → Bool

/-- A decide respects the principal boundary iff it factors through
    `b.principal` — two behaviors with the same principal always reach
    the same decision for any permission. -/
def RespectsPrincipal {Permission : Type} (decide : Decide Permission) : Prop :=
  ∀ (b₁ b₂ : Behavior) (p : Permission),
    b₁.principal = b₂.principal → decide b₁ p = decide b₂ p

/-- Canonical decide: a behavior is allowed iff its principal is
    granted. Proves `RespectsPrincipal` is inhabited. -/
def canonicalDecide {Permission : Type} (g : GrantStore Permission) :
    Decide Permission :=
  fun b p => g.granted b.principal p

end Identity
