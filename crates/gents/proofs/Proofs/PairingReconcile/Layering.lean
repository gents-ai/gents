import Proofs.Basic
import Mathlib.Data.Finset.Basic

namespace PairingReconcile.Layering

structure Layer where
  subscriptions : Finset String
  replicatorCollections : Finset String
  isAppCollections : Bool
  deriving DecidableEq

def clampDataPlane (l : Layer) : Layer :=
  if l.isAppCollections then l else { l with subscriptions := (∅ : Finset String) }

def mergeLayered (base : Option Layer) (dataPlane : Option Layer) : Option Layer :=
  match base, dataPlane.map clampDataPlane with
  | none, none => none
  | some b, none => some b
  | none, some d => some d
  | some b, some d =>
      some { subscriptions := b.subscriptions ∪ d.subscriptions
           , replicatorCollections := b.replicatorCollections ∪ d.replicatorCollections
           , isAppCollections := b.isAppCollections || d.isAppCollections }

theorem appCollections_subscription_survives
    (base : Option Layer) (d : Layer) (h : d.isAppCollections = true)
    (m : Layer) (hm : mergeLayered base (some d) = some m) :
    d.subscriptions ⊆ m.subscriptions := by
  cases base with
  | none =>
      simp [mergeLayered, clampDataPlane, h] at hm
      subst hm
      exact Finset.Subset.refl _
  | some b =>
      simp [mergeLayered, clampDataPlane, h] at hm
      subst hm
      exact Finset.subset_union_right

theorem nonApp_none_base_no_subscription
    (d : Layer) (h : d.isAppCollections = false) :
    mergeLayered none (some d) = some { d with subscriptions := (∅ : Finset String) } := by
  simp [mergeLayered, clampDataPlane, h]

theorem nonApp_subscription_eq_base
    (b d : Layer) (h : d.isAppCollections = false)
    (m : Layer) (hm : mergeLayered (some b) (some d) = some m) :
    m.subscriptions = b.subscriptions := by
  simp [mergeLayered, clampDataPlane, h] at hm
  subst hm
  simp

theorem base_preserved
    (b : Layer) (dp : Option Layer) (m : Layer)
    (hm : mergeLayered (some b) dp = some m) :
    b.subscriptions ⊆ m.subscriptions ∧ b.replicatorCollections ⊆ m.replicatorCollections := by
  cases dp with
  | none =>
      simp [mergeLayered] at hm
      subst hm
      exact ⟨Finset.Subset.refl _, Finset.Subset.refl _⟩
  | some d =>
      cases h : d.isAppCollections with
      | false =>
          simp [mergeLayered, clampDataPlane, h] at hm
          subst hm
          exact ⟨by simp, by simp [Finset.subset_union_left]⟩
      | true =>
          simp [mergeLayered, clampDataPlane, h] at hm
          subst hm
          exact ⟨Finset.subset_union_left, Finset.subset_union_left⟩

end PairingReconcile.Layering
