import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image

/-!
# Identity — State

Records and well-formedness for the `AgentPrincipal` /
`AgentBehavior` / `AgentDeployment` split (#185).
-/

namespace Identity

abbrev DID := String
abbrev BehaviorId := String
abbrev DeploymentId := String

structure Principal where
  did         : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

structure Behavior where
  id          : BehaviorId
  principal   : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

structure Deployment where
  id        : DeploymentId
  principal : DID
  hostId    : String
  enabled   : Bool
  deriving DecidableEq, Repr

structure World where
  principals  : Finset Principal
  behaviors   : Finset Behavior
  deployments : Finset Deployment

def World.WellFormed (w : World) : Prop :=
  (∀ p₁ p₂ : Principal, p₁ ∈ w.principals → p₂ ∈ w.principals →
      p₁.did = p₂.did → p₁ = p₂) ∧
  (∀ b₁ b₂ : Behavior, b₁ ∈ w.behaviors → b₂ ∈ w.behaviors →
      b₁.id = b₂.id → b₁ = b₂) ∧
  (∀ d₁ d₂ : Deployment, d₁ ∈ w.deployments → d₂ ∈ w.deployments →
      d₁.id = d₂.id → d₁ = d₂) ∧
  (∀ b : Behavior, b ∈ w.behaviors →
      b.principal ∈ w.principals.image (·.did)) ∧
  (∀ d : Deployment, d ∈ w.deployments →
      d.principal ∈ w.principals.image (·.did))

end Identity
