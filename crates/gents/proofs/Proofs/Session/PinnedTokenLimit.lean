import Lean

namespace Request.PinnedTokenLimit
open Lean

structure Input where
  limit : Option Int
  deriving ToJson

structure Outcome where
  accepted : Bool
  limit : Option Nat
  error : Option String
  deriving ToJson

/-- A pinned durable request may have exhausted its allowance exactly to zero.
Negative stored integers are invalid and the rejection names why; absence means
there is no aggregate cap. -/
def parse (input : Input) : Outcome :=
  match input.limit with
  | none => ⟨true, none, none⟩
  | some value =>
    if value < 0 then ⟨false, none, some "pinned max_total_tokens must not be negative"⟩
    else ⟨true, some value.toNat, none⟩

end Request.PinnedTokenLimit
