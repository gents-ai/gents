import Proofs.ApplyReconcile.Collections

namespace ApplyReconcile

structure DesiredFields where
  content : String
  refs    : Finset DocRef
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

def referencesOf : DesiredFields → Finset DocRef := fun f => f.refs

def Manifest.WellFormed (m : Manifest) : Prop :=
  (∀ d : DocRef, ∀ f, m.docs d = some f →
    ∀ r ∈ referencesOf f, m.contains r = true) ∧
  (∀ d : DocRef, ∀ f, m.docs d = some f →
    ∀ r ∈ referencesOf f,
      r.collection.applyOrder < d.collection.applyOrder)

def LiveState.WellFormed (L : LiveState) : Prop :=
  (∀ d : DocRef, ∀ f, L.desired d = some f →
    ∀ r ∈ referencesOf f, L.contains r = true) ∧
  (∀ d : DocRef, ∀ f, L.desired d = some f →
    ∀ r ∈ referencesOf f,
      r.collection.applyOrder < d.collection.applyOrder)

end ApplyReconcile
