import Lean

namespace Request.PinnedTokenLimit
open Lean

structure Input where
  limit : Option Int
  deriving ToJson

structure Outcome where
  accepted : Bool
  limit : Option Nat
  deriving ToJson

/-- A pinned durable request may have exhausted its allowance exactly to zero.
Negative stored integers are invalid; absence means there is no aggregate cap. -/
def parse (input : Input) : Outcome :=
  match input.limit with
  | none => ⟨true, none⟩
  | some value => if value < 0 then ⟨false, none⟩ else ⟨true, some value.toNat⟩

end Request.PinnedTokenLimit
