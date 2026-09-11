import Proofs.RuntimeReconcile
import Mathlib.Data.Finset.Image
import Mathlib.Data.Finset.SDiff
import Proofs.ApplyReconcile.Manifest
import Proofs.Configuration

namespace ApplyReconcile

/-- The loader's canonical decoding boundary. This model proves how the decoded
registry is used, not that the still-unmigrated Rust decoder implements it. The
input is desired configuration only; live observations have a separate owner. -/
abbrev RegistryDecoder := (DocRef → Option DesiredFields) → Configuration.Registry

/-- Preserve authored IDs. The lifecycle model uses abstract Nat identities;
callers must supply an injective encoding when relating those identities to names. -/
def Manifest.behaviorNames (m : Manifest) (owner : String) : Finset String :=
  (m.support.filter (fun d => d.collection = .agentBehavior ∧ d.agentDid = owner)).image DocRef.id

/-- Equal labels from other principals cannot replace this owner's lookup. -/
theorem mem_behaviorNames_iff (m : Manifest) (owner name : String) :
    name ∈ m.behaviorNames owner ↔ (m.docs ⟨.agentBehavior, name, owner⟩).isSome = true := by
  simp only [Manifest.behaviorNames, Finset.mem_image, Finset.mem_filter]
  constructor
  · rintro ⟨⟨collection, id, agent⟩, ⟨hm, hc, ho⟩, hn⟩
    cases hc
    cases hn
    cases ho
    exact (m.support_iff _).mp hm
  · intro h
    exact ⟨⟨.agentBehavior, name, owner⟩, ⟨(m.support_iff _).mpr h, rfl, rfl⟩, rfl⟩

/-- A present behavior is configuration-ready only when the common resolver
produces its complete context and inference selection. -/
def configurationReady (desired : DocRef → Option DesiredFields)
    (decode : RegistryDecoder) (owner name : String) : Prop :=
  (desired ⟨.agentBehavior, name, owner⟩).isSome = true ∧
    (Configuration.resolveBehavior (decode desired) owner name).toOption.isSome = true

instance (desired : DocRef → Option DesiredFields) (decode : RegistryDecoder)
    (owner name : String) : Decidable (configurationReady desired decode owner name) := by
  unfold configurationReady
  infer_instance

def resolvedBehaviorIds (desired : DocRef → Option DesiredFields)
    (decode : RegistryDecoder) (owner : String) (names : Finset String)
    (encode : String → BehaviorId) : Finset BehaviorId :=
  (names.filter (configurationReady desired decode owner)).image encode

/-- Runtime availability remains an observation of its existing owner. Successful
configuration resolution supplies dependencies; it does not invent availability. -/
def snapshotFromDesired (desired : DocRef → Option DesiredFields)
    (decode : RegistryDecoder) (owner : String) (names : Finset String)
    (encode : String → BehaviorId) (_hencode : Function.Injective encode)
    (defaultBehavior : BehaviorId)
    (runtimeAvailable : Finset BehaviorId) : ResolvedSnapshot :=
  let dependencies := resolvedBehaviorIds desired decode owner names encode
  let runnable := dependencies ∩ runtimeAvailable
  { defaultBehavior
    runnable
    unavailable := names.image encode \ runnable
    dependenciesSatisfied := dependencies }

def LiveState.toResolvedSnapshot (L : LiveState)
    (decode : RegistryDecoder) (owner : String) (names : Finset String)
    (encode : String → BehaviorId) (hencode : Function.Injective encode)
    (defaultBehavior : BehaviorId)
    (runtimeAvailable : Finset BehaviorId) : ResolvedSnapshot :=
  snapshotFromDesired L.desired decode owner names encode hencode defaultBehavior runtimeAvailable

/-- Injectivity prevents different authored names from sharing a lifecycle identity. -/
theorem resolved_id_iff (desired : DocRef → Option DesiredFields)
    (decode : RegistryDecoder) (owner name : String) (names : Finset String)
    (encode : String → BehaviorId) (hencode : Function.Injective encode) :
    encode name ∈ resolvedBehaviorIds desired decode owner names encode ↔
      name ∈ names ∧ configurationReady desired decode owner name := by
  simp only [resolvedBehaviorIds, Finset.mem_image, Finset.mem_filter]
  constructor
  · rintro ⟨other, ⟨hn, hr⟩, he⟩
    have heq := hencode he
    subst other
    exact ⟨hn, hr⟩
  · rintro ⟨hn, hr⟩
    exact ⟨name, ⟨hn, hr⟩, rfl⟩

theorem runnable_iff (desired : DocRef → Option DesiredFields)
    (decode : RegistryDecoder) (owner name : String) (names : Finset String)
    (encode : String → BehaviorId) (hencode : Function.Injective encode)
    (defaultBehavior : BehaviorId) (runtimeAvailable : Finset BehaviorId) :
    encode name ∈ (snapshotFromDesired desired decode owner names encode hencode
      defaultBehavior runtimeAvailable).runnable ↔
      (name ∈ names ∧ configurationReady desired decode owner name) ∧
        encode name ∈ runtimeAvailable := by
  simp only [snapshotFromDesired, Finset.mem_inter,
    resolved_id_iff desired decode owner name names encode hencode]

/-- Missing, invalid, foreign-owned, and disabled configurations fail closed via
exactly the common resolver; document presence cannot override its rejection. -/
theorem rejected_configuration_not_runnable (desired : DocRef → Option DesiredFields)
    (decode : RegistryDecoder) (owner name : String) (names : Finset String)
    (encode : String → BehaviorId) (hencode : Function.Injective encode)
    (defaultBehavior : BehaviorId) (runtimeAvailable : Finset BehaviorId)
    (err : Configuration.ResolveError)
    (h : Configuration.resolveBehavior (decode desired) owner name = .error err) :
    encode name ∉ (snapshotFromDesired desired decode owner names encode hencode
      defaultBehavior runtimeAvailable).runnable := by
  rw [runnable_iff desired decode owner name names encode hencode]
  simp [configurationReady, h, Except.toOption]

theorem snapshot_coverage (desired : DocRef → Option DesiredFields)
    (decode : RegistryDecoder) (owner : String) (names : Finset String)
    (encode : String → BehaviorId) (hencode : Function.Injective encode)
    (defaultBehavior : BehaviorId)
    (runtimeAvailable : Finset BehaviorId) :
    let snapshot := snapshotFromDesired desired decode owner names encode hencode
      defaultBehavior runtimeAvailable
    snapshot.runnable ∪ snapshot.unavailable = names.image encode := by
  apply Finset.union_sdiff_of_subset
  exact Finset.Subset.trans Finset.inter_subset_left
    (Finset.image_subset_image (Finset.filter_subset _ _))

/-- The resolver-backed partition satisfies the existing activation contract.
A configured default must belong to the classified behavior set; it need not be
ready when its configuration or runtime resources are unavailable. -/
theorem snapshot_wellFormed (desired : DocRef → Option DesiredFields)
    (decode : RegistryDecoder) (owner : String) (names : Finset String)
    (encode : String → BehaviorId) (hencode : Function.Injective encode)
    (defaultBehavior : BehaviorId) (runtimeAvailable : Finset BehaviorId)
    (hdefault : defaultBehavior ∈ names.image encode) :
    (snapshotFromDesired desired decode owner names encode hencode
      defaultBehavior runtimeAvailable).wellFormed := by
  refine ⟨?_, Finset.inter_subset_left, ?_⟩
  · apply Finset.disjoint_left.mpr
    intro bid hrun hunavailable
    exact (Finset.mem_sdiff.mp hunavailable).2 hrun
  · rw [snapshot_coverage desired decode owner names encode hencode
      defaultBehavior runtimeAvailable]
    exact hdefault

end ApplyReconcile
