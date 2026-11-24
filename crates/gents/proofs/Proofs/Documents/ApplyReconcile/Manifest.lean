import Proofs.Documents.ApplyReconcile.Collections
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

def empty : Manifest := ⟨fun _ => none, ∅, by simp⟩

theorem ext {m n : Manifest} (h : m.docs = n.docs) : m = n := by
  cases m with
  | mk docs support support_iff =>
    cases n with
    | mk docs' support' support_iff' =>
      simp only at h
      subst h
      have : support = support' := by
        ext d
        rw [support_iff, support_iff']
      subst this
      rfl

end Manifest

/-- The stored desired documents of every owner and the live observations kept
beside them by their own owners. -/
structure LiveState where
  desired : Manifest
  live    : DocRef → Option LiveFields

namespace LiveState

def contains (L : LiveState) (d : DocRef) : Bool := L.desired.contains d

end LiveState

end ApplyReconcile
