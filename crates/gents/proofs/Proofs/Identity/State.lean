import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image

namespace Identity

abbrev DID := String
abbrev AgentId := String

structure Node where
  did         : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

structure Agent where
  id          : AgentId
  node        : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

structure World where
  nodes      : Finset Node
  agents     : Finset Agent

def World.WellFormed (w : World) : Prop :=
  (∀ p₁ ∈ w.nodes, ∀ p₂ ∈ w.nodes,
      p₁.did = p₂.did → p₁ = p₂) ∧
  (∀ b₁ ∈ w.agents, ∀ b₂ ∈ w.agents,
      b₁.node = b₂.node → b₁.id = b₂.id → b₁ = b₂) ∧
  (∀ b : Agent, b ∈ w.agents →
      b.node ∈ w.nodes.image (·.did))

instance (w : World) : Decidable w.WellFormed := by
  unfold World.WellFormed
  infer_instance

/-- Resolution requires both owner and logical label. Invalid duplicate keys
fail rather than allowing list order to select a competing document. -/
def findAgent? (agents : List Agent) (node : DID) (id : AgentId) : Option Agent :=
  match agents.filter (fun b => b.node == node && b.id == id) with
  | [agent] => some agent
  | _ => none

end Identity
