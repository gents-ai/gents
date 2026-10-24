import Proofs.SessionHydration.FileInventory

namespace SessionHydration.FileInventory.Order

open CanonicalOutput


/-- All windows come from one authorized snapshot and its target-session
coordinate space. Selecting a resource never turns an unknown capture into
known coverage, and this result grants no retained-image access. -/
def comparison (snapshot : Snapshot) (input : ClosureInput)
    (node root path : String) : Option (List DocId) := do
  if input.request != snapshot.admission || [node, root, path].any String.isEmpty then
    none
  else pure ()
  let observed := snapshot.tools.map fun fact => window snapshot input fact.tool
  if observed != snapshot.ordering then none else pure ()
  snapshot.orderedTools node root path

theorem foreign_closure_cannot_order (snapshot : Snapshot) (input : ClosureInput)
    (node root path : String) (h : input.request ≠ snapshot.admission) :
    comparison snapshot input node root path = none := by
  simp [comparison, h]

end SessionHydration.FileInventory.Order
