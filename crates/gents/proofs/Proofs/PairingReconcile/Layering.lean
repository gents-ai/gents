import Proofs.PairingReconcile.State

namespace PairingReconcile.Layering

structure Layer where
  subscriptions : Finset String
  replicatorCollections : Finset String
  filters : ReplicatorFilter := ∅
  deriving DecidableEq

def materializeBase (localDid peerDid : String) (layer : Layer) : Option Layer :=
  if peerDid = localDid then none else some layer

/-- A data-plane layer replicates but never subscribes.

Decision (supported topology, 2026-11-07): P2P carries only discovery and
enrollment, and no surviving scope template is a subscribe template, so the
former app-collections exception is gone; conversation data is read through
native ACP and GraphQL instead of template replication. -/
def clampDataPlane (l : Layer) : Layer :=
  { l with subscriptions := (∅ : Finset String) }

def mergeFilters (base dataPlane : Layer) : ReplicatorFilter :=
  base.filters.filter (fun baseFilter =>
    ∀ dataFilter ∈ dataPlane.filters, dataFilter.collection ≠ baseFilter.collection) ∪
    dataPlane.filters

def mergeMaterialized (base : Option Layer) (dataPlane : Option Layer) : Option Layer :=
  match base, dataPlane.map clampDataPlane with
  | none, none => none
  | some b, none => some b
  | none, some d => some d
  | some b, some d =>
      some { subscriptions := b.subscriptions ∪ d.subscriptions
           , replicatorCollections := b.replicatorCollections ∪ d.replicatorCollections
           , filters := mergeFilters b d }

def mergeLayered (localDid peerDid : String) (base : Option Layer)
    (dataPlane : Option Layer) : Option Layer :=
  mergeMaterialized (base.bind (materializeBase localDid peerDid)) dataPlane

theorem self_pairing_is_not_materialized (localDid : String) (layer : Layer) :
    mergeLayered localDid localDid (some layer) none = none := by
  simp [mergeLayered, mergeMaterialized, materializeBase]

theorem data_plane_none_base_no_subscription (d : Layer) :
    mergeMaterialized none (some d) = some { d with subscriptions := (∅ : Finset String) } := by
  simp [mergeMaterialized, clampDataPlane]

theorem data_plane_subscription_eq_base
    (b d : Layer) (m : Layer) (hm : mergeMaterialized (some b) (some d) = some m) :
    m.subscriptions = b.subscriptions := by
  simp [mergeMaterialized, clampDataPlane] at hm
  subst hm
  simp

theorem base_preserved
    (b : Layer) (dp : Option Layer) (m : Layer)
    (hm : mergeMaterialized (some b) dp = some m) :
    b.subscriptions ⊆ m.subscriptions ∧ b.replicatorCollections ⊆ m.replicatorCollections := by
  cases dp with
  | none =>
      simp [mergeMaterialized] at hm
      subst hm
      exact ⟨Finset.Subset.refl _, Finset.Subset.refl _⟩
  | some d =>
      simp [mergeMaterialized, clampDataPlane] at hm
      subst hm
      exact ⟨by simp, by simp [Finset.subset_union_left]⟩

theorem data_plane_filters_preserved
    (b d m : Layer) (hm : mergeMaterialized (some b) (some d) = some m) :
    d.filters ⊆ m.filters := by
  simp [mergeMaterialized, clampDataPlane] at hm
  subst hm
  exact Finset.subset_union_right

theorem overridden_base_filter_is_removed
    (b d m : Layer) (baseFilter dataFilter : CollectionFilterKey)
    (hm : mergeMaterialized (some b) (some d) = some m)
    (hdata : dataFilter ∈ d.filters) (hnotData : baseFilter ∉ d.filters)
    (hoverlap : baseFilter.collection = dataFilter.collection) :
    baseFilter ∉ m.filters ∧ dataFilter ∈ m.filters := by
  have hremoved : baseFilter ∉ mergeFilters b d := by
    simp only [mergeFilters, Finset.mem_union, Finset.mem_filter]
    rintro (⟨_, hall⟩ | hmember)
    · exact (hall dataFilter hdata) hoverlap.symm
    · exact hnotData hmember
  have hpreserved : dataFilter ∈ mergeFilters b d :=
    Finset.mem_union_right _ hdata
  simp [mergeMaterialized, clampDataPlane] at hm
  subst hm
  exact ⟨hremoved, hpreserved⟩

end PairingReconcile.Layering
