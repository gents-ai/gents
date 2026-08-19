import Proofs.PairingReconcile.State

namespace PairingReconcile.Layering

structure Layer where
  subscriptions : Finset String
  replicatorCollections : Finset String
  filters : ReplicatorFilter := ∅
  isAppCollections : Bool
  deriving DecidableEq

def materializeBase (localDid peerDid : String) (layer : Layer) : Option Layer :=
  if peerDid = localDid then none else some layer

theorem self_pairing_is_not_materialized (localDid : String) (layer : Layer) :
    materializeBase localDid localDid layer = none := by
  simp [materializeBase]

def clampDataPlane (l : Layer) : Layer :=
  if l.isAppCollections then l else { l with subscriptions := (∅ : Finset String) }

def mergeFilters (base dataPlane : Layer) : ReplicatorFilter :=
  base.filters ∪ dataPlane.filters.filter (fun f =>
    f.collection ≠ "BearerPairingReady" ∨
      ∀ baseFilter ∈ base.filters, baseFilter.collection ≠ "BearerPairingReady")

def mergeLayered (base : Option Layer) (dataPlane : Option Layer) : Option Layer :=
  match base, dataPlane.map clampDataPlane with
  | none, none => none
  | some b, none => some b
  | none, some d => some d
  | some b, some d =>
      some { subscriptions := b.subscriptions ∪ d.subscriptions
           , replicatorCollections := b.replicatorCollections ∪ d.replicatorCollections
           , filters := mergeFilters b d
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

/-- Data-plane predicates survive unless a control-plane readiness predicate
    already names the claimant. Other overlaps remain conjunctive. -/
theorem data_plane_filters_preserved
    (b d m : Layer) (hm : mergeLayered (some b) (some d) = some m)
    (hready : ∀ f ∈ d.filters, f.collection = "BearerPairingReady" →
      ∀ baseFilter ∈ b.filters, baseFilter.collection ≠ "BearerPairingReady") :
    d.filters ⊆ m.filters := by
  cases h : d.isAppCollections <;>
    simp [mergeLayered, clampDataPlane, h] at hm <;>
    subst hm <;>
    intro f hf <;>
    simp only [mergeFilters, Finset.mem_union, Finset.mem_filter] <;>
    exact Or.inr ⟨hf, by
      by_cases hc : f.collection = "BearerPairingReady"
      · exact Or.inr (hready f hf hc)
      · exact Or.inl hc⟩

theorem overlapping_collection_filters_compose
    (b d m : Layer) (baseFilter dataFilter : CollectionFilterKey)
    (hm : mergeLayered (some b) (some d) = some m)
    (hbase : baseFilter ∈ b.filters) (hdata : dataFilter ∈ d.filters)
    (hnotReady : dataFilter.collection ≠ "BearerPairingReady")
    (_hoverlap : baseFilter.collection = dataFilter.collection) :
    baseFilter ∈ m.filters ∧ dataFilter ∈ m.filters := by
  refine ⟨base_filters_preserved b (some d) m hm hbase, ?_⟩
  cases h : d.isAppCollections <;>
    simp [mergeLayered, clampDataPlane, h] at hm <;>
    subst hm <;>
    exact Finset.mem_union_right _ (Finset.mem_filter.mpr ⟨hdata, Or.inl hnotReady⟩)

theorem base_readiness_filter_is_authoritative
    (b d m : Layer) (baseFilter : CollectionFilterKey)
    (hm : mergeLayered (some b) (some d) = some m)
    (hbase : baseFilter ∈ b.filters)
    (hready : baseFilter.collection = "BearerPairingReady") :
    m.filters.filter (fun f => f.collection = "BearerPairingReady") =
      b.filters.filter (fun f => f.collection = "BearerPairingReady") := by
  have hmerge :
      (mergeFilters b d).filter (fun f => f.collection = "BearerPairingReady") =
        b.filters.filter (fun f => f.collection = "BearerPairingReady") := by
    ext f
    simp only [mergeFilters, Finset.mem_filter, Finset.mem_union]
    constructor
    · rintro ⟨hf | ⟨_hdata, hallowed⟩, hcollection⟩
      · exact ⟨hf, hcollection⟩
      · rcases hallowed with hnot | hall
        · exact False.elim (hnot hcollection)
        · exact False.elim (hall baseFilter hbase hready)
    · rintro ⟨hf, hcollection⟩
      exact ⟨Or.inl hf, hcollection⟩
  cases h : d.isAppCollections <;>
    simp [mergeLayered, clampDataPlane, h] at hm <;>
    subst hm <;>
    exact hmerge

end PairingReconcile.Layering
