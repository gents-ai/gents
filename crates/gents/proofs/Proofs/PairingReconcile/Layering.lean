import Proofs.PairingReconcile.State

namespace PairingReconcile.Layering

structure Layer where
  subscriptions : Finset String
  replicatorCollections : Finset String
  filters : ReplicatorFilter := ∅
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
           , filters := b.filters ∪ d.filters
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

/-- Layering is conjunctive: no base predicate can be overwritten by a
    data-plane predicate on the same collection. -/
theorem base_filters_preserved
    (b : Layer) (dp : Option Layer) (m : Layer)
    (hm : mergeLayered (some b) dp = some m) :
    b.filters ⊆ m.filters := by
  cases dp with
  | none =>
      simp [mergeLayered] at hm
      subst hm
      exact Finset.Subset.refl _
  | some d =>
      cases h : d.isAppCollections <;>
        simp [mergeLayered, clampDataPlane, h] at hm <;>
        subst hm <;> exact Finset.subset_union_left

/-- Every data-plane predicate also survives a two-layer merge. Together with
    `base_filters_preserved`, overlapping collection predicates are ANDed. -/
theorem data_plane_filters_preserved
    (b d m : Layer) (hm : mergeLayered (some b) (some d) = some m) :
    d.filters ⊆ m.filters := by
  cases h : d.isAppCollections <;>
    simp [mergeLayered, clampDataPlane, h] at hm <;>
    subst hm <;> exact Finset.subset_union_right

theorem overlapping_collection_filters_compose
    (b d m : Layer) (baseFilter dataFilter : CollectionFilterKey)
    (hm : mergeLayered (some b) (some d) = some m)
    (hbase : baseFilter ∈ b.filters) (hdata : dataFilter ∈ d.filters)
    (_hoverlap : baseFilter.collection = dataFilter.collection) :
    baseFilter ∈ m.filters ∧ dataFilter ∈ m.filters := by
  exact ⟨base_filters_preserved b (some d) m hm hbase,
    data_plane_filters_preserved b d m hm hdata⟩

end PairingReconcile.Layering
