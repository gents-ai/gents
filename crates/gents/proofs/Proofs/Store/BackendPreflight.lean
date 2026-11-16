import Lean

namespace BackendPreflight

inductive CopyResult where
  | compatible | incompatible | unavailable
  deriving BEq, DecidableEq, Repr, Lean.ToJson

/-- A failed inspection of a copy is not permission to open the original.
Only an observed compatible copy permits the protected store to open. -/
def decision : CopyResult → String
  | .compatible => "open"
  | .incompatible => "incompatible"
  | .unavailable => "unavailable"

theorem unavailable_never_opens : decision .unavailable ≠ "open" := by decide
theorem incompatible_never_opens : decision .incompatible ≠ "open" := by decide

end BackendPreflight
