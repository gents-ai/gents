import Proofs.ApplyReconcile.ApplyProperties

namespace ApplyReconcile

def DocRef.behaviorId? : DocRef → Option BehaviorId := fun d =>
  match d.collection with
  | .agentBehavior => some d.id.length
  | _ => none

noncomputable def Manifest.behaviorIds (m : Manifest) : Finset BehaviorId :=
  (m.support.filter (fun d => d.collection = .agentBehavior)).image
    (fun d => d.id.length)

noncomputable def LiveState.toResolvedSnapshot
    (L : LiveState) (defaultBehavior : BehaviorId)
    (allBehaviors : Finset BehaviorId) : ResolvedSnapshot :=
  let runnable : Finset BehaviorId :=
    @Finset.filter _ (fun bid =>
        ∃ d : DocRef, d.behaviorId? = some bid ∧ L.contains d = true)
      (Classical.decPred _) allBehaviors
  { defaultBehavior := defaultBehavior
  , runnable := runnable
  , unavailable := allBehaviors \ runnable
  , dependenciesSatisfied := runnable }

lemma LiveState.toResolvedSnapshot_coverage
    (L : LiveState) (defaultBehavior : BehaviorId)
    (allBehaviors : Finset BehaviorId) :
    (L.toResolvedSnapshot defaultBehavior allBehaviors).runnable ∪
      (L.toResolvedSnapshot defaultBehavior allBehaviors).unavailable =
        allBehaviors := by
  unfold LiveState.toResolvedSnapshot
  simp only
  apply Finset.union_sdiff_of_subset
  exact @Finset.filter_subset _ _ (Classical.decPred _) allBehaviors

end ApplyReconcile
