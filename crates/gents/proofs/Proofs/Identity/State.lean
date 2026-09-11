import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image

namespace Identity

abbrev DID := String
abbrev BehaviorId := String

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

structure World where
  principals  : Finset Principal
  behaviors   : Finset Behavior

def World.WellFormed (w : World) : Prop :=
  (∀ p₁ ∈ w.principals, ∀ p₂ ∈ w.principals,
      p₁.did = p₂.did → p₁ = p₂) ∧
  (∀ b₁ ∈ w.behaviors, ∀ b₂ ∈ w.behaviors,
      b₁.principal = b₂.principal → b₁.id = b₂.id → b₁ = b₂) ∧
  (∀ b : Behavior, b ∈ w.behaviors →
      b.principal ∈ w.principals.image (·.did))

instance (w : World) : Decidable w.WellFormed := by
  unfold World.WellFormed
  infer_instance

/-- Resolution requires both owner and logical label. Invalid duplicate keys
fail rather than allowing list order to select a competing document. -/
def findBehavior? (behaviors : List Behavior) (principal : DID) (id : BehaviorId) : Option Behavior :=
  match behaviors.filter (fun b => b.principal == principal && b.id == id) with
  | [behavior] => some behavior
  | _ => none

end Identity
