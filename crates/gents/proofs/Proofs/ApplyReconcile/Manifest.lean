import Proofs.ApplyReconcile.Collections
import Mathlib.Data.Finset.Basic

namespace ApplyReconcile

structure DesiredFields where
  content : String
  refs    : List DocRef
  deriving DecidableEq

abbrev LiveFields := String

structure Manifest where
  docs        : DocRef → Option DesiredFields
  support     : Finset DocRef
  support_iff : ∀ d, d ∈ support ↔ (docs d).isSome = true

namespace Manifest

def contains (m : Manifest) (d : DocRef) : Bool := (m.docs d).isSome

end Manifest

structure LiveState where
  desired : DocRef → Option DesiredFields
  live    : DocRef → Option LiveFields

namespace LiveState

def contains (L : LiveState) (d : DocRef) : Bool := (L.desired d).isSome

end LiveState

end ApplyReconcile
