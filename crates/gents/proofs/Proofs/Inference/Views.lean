import Proofs.Store.Views

namespace InferenceViews

structure Exposure where
  backend : Bool
  observation : Bool
  deriving BEq, Repr

def exposure (metadataReadable observationReadable ownNode : Bool) : Exposure :=
  let node := if ownNode then "own" else "other"
  let backend := ReadViews.visible .ownNode "own" "reader" node "" metadataReadable
  let observation := ReadViews.visible .ownNode "own" "reader" node "" observationReadable
  { backend, observation := backend && observation }

theorem backend_requires_reader_access (metadataReadable observationReadable ownNode : Bool)
    (h : (exposure metadataReadable observationReadable ownNode).backend = true) :
    metadataReadable = true := by
  cases metadataReadable <;> cases observationReadable <;> cases ownNode <;>
    simp_all [exposure, ReadViews.visible, ReadViews.scopeMatches]

theorem joined_observation_requires_both_grants
    (metadataReadable observationReadable ownNode : Bool)
    (h : (exposure metadataReadable observationReadable ownNode).observation = true) :
    metadataReadable = true ∧ observationReadable = true := by
  cases metadataReadable <;> cases observationReadable <;> cases ownNode <;>
    simp_all [exposure, ReadViews.visible, ReadViews.scopeMatches]

theorem foreign_node_exposes_nothing (metadataReadable observationReadable : Bool) :
    exposure metadataReadable observationReadable false = ⟨false, false⟩ := by
  cases metadataReadable <;> cases observationReadable <;>
    rfl

end InferenceViews
