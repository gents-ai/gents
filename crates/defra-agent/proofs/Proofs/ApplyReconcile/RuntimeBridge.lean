import Proofs.ApplyReconcile.ApplyProperties

/-!
# Apply/Reconcile Runtime Bridge

Projection from post-apply live state into runtime resolved snapshots.
-/

namespace ApplyReconcile

/-! ## Bridge to `RuntimeReconcile.ResolvedSnapshot`

The runtime's control watcher + resolver ultimately publish a
`ResolvedSnapshot` whose `runnable ∪ unavailable` is the set of
behavior ids declared by the manifest. This module defines the structural
bridge from `Manifest` / `LiveState` to `ResolvedSnapshot` and the
coverage lemma consumed by the T-Conv corollaries.
-/

/-- If this DocRef names an agent behavior, project out a stable
    `BehaviorId`. The model treats `BehaviorId` as `Nat`; we use the
    id string's length as a placeholder mapping. The proofs are
    mapping-agnostic — any total `String → BehaviorId` function works;
    only totality matters for the coverage lemma. -/
def DocRef.behaviorId? : DocRef → Option BehaviorId := fun d =>
  match d.collection with
  | .agentBehavior => some d.id.length
  | _ => none

/-- The set of behavior ids declared by this manifest, derived by
    filtering `M.support` to `agentBehavior` DocRefs and mapping the
    placeholder id→Nat. Concrete mapping choice does not matter for
    the proofs that consume this set. -/
noncomputable def Manifest.behaviorIds (m : Manifest) : Finset BehaviorId :=
  (m.support.filter (fun d => d.collection = .agentBehavior)).image
    (fun d => d.id.length)

/-- Bridge from a post-apply `LiveState` + caller-supplied default
    behavior + base behavior set into a `ResolvedSnapshot`. Follows the
    runtime's reconcile semantics at an abstract level:
    - `runnable := allBehaviors.filter (· has a matching present DocRef in L)`
    - `unavailable := allBehaviors \ runnable`

    `defaultBehavior` is a caller parameter because the model's abstract
    `DesiredFields` is opaque — the specific `AgentPrincipal.default_behavior_id`
    field isn't named in the Lean inductive. The topological invariant
    (Manifest.WellFormed's second conjunct, `refs go to strictly-lower-rank
    collections`) does cover principal→behavior references: when a Manifest
    carries an AgentPrincipal DocRef whose DesiredFields.refs includes an
    AgentBehavior DocRef, the sort in `diff` writes the behavior first
    (rank 1) before the principal (rank 3). This matches production's
    control-watcher semantics: a principal whose default_behavior is not yet
    visible is held in `PendingVisibility` until the behavior write lands.
    Concrete reference populations (including default_behavior_id edges)
    are exercised in the Rust conformance tests.

    Uses classical decidability on the existential predicate — the
    proofs that consume this snapshot (notably `toResolvedSnapshot_coverage`)
    do not require the predicate to be computable; only that runnable
    is a subset of `allBehaviors`, which follows from `Finset.filter_subset`. -/
noncomputable def LiveState.toResolvedSnapshot
    (L : LiveState) (defaultBehavior : BehaviorId)
    (allBehaviors : Finset BehaviorId) : ResolvedSnapshot :=
  let runnable : Finset BehaviorId :=
    @Finset.filter _ (fun bid =>
        ∃ d : DocRef, d.behaviorId? = some bid ∧ L.contains d = true)
      (Classical.decPred _) allBehaviors
  { defaultBehavior := defaultBehavior
  , runnable := runnable
  , unavailable := allBehaviors \ runnable }

/-- Coverage: the resolved snapshot's `runnable` and `unavailable` sets
    together cover the supplied base behavior set. This is the structural
    coverage form used by the T-Conv corollaries. -/
lemma LiveState.toResolvedSnapshot_coverage
    (L : LiveState) (defaultBehavior : BehaviorId)
    (allBehaviors : Finset BehaviorId) :
    (L.toResolvedSnapshot defaultBehavior allBehaviors).runnable ∪
      (L.toResolvedSnapshot defaultBehavior allBehaviors).unavailable =
        allBehaviors := by
  unfold LiveState.toResolvedSnapshot
  simp only
  -- Goal: (allBehaviors.filter p) ∪ (allBehaviors \ allBehaviors.filter p) = allBehaviors.
  -- The filter is a subset of allBehaviors, so this is `union_sdiff_of_subset`.
  apply Finset.union_sdiff_of_subset
  exact @Finset.filter_subset _ _ (Classical.decPred _) allBehaviors

end ApplyReconcile
