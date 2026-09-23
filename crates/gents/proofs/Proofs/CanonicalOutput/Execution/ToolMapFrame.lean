import Proofs.CanonicalOutput.Execution.RequestCoherence

namespace CanonicalOutput.Execution

/-- Key-preserving edits commute with singleton lookup, including its rejection
of ambiguous keys. No uniqueness assumption is needed for this frame. -/
theorem filter_map_key {α κ : Type} [BEq κ] (rows : List α)
    (key : α → κ) (document : κ) (edit : α → α)
    (stable : ∀ row, key (edit row) = key row) :
    (rows.map edit).filter (fun row => key row == document) =
      (rows.filter (fun row => key row == document)).map edit := by
  induction rows with
  | nil => rfl
  | cons head tail ih =>
    simp only [List.map_cons, List.filter_cons, stable]
    split <;> simp_all

theorem replaceOwnedTool_eq_map (world : World) (selected : OwnedTool)
    (edit : OwnedTool → OwnedTool)
    (unique : (world.toolContexts.map (·.document)).Nodup)
    (member : selected ∈ world.toolContexts) :
    replaceOwnedTool world.toolContexts selected.document (edit selected) =
      world.toolContexts.map (fun tool => if tool.document == selected.document then edit tool else tool) := by
  apply List.map_congr_left
  intro tool hm
  split
  · rename_i heq
    have eq : tool = selected := by
      exact List.inj_on_of_nodup_map unique hm member (beq_iff_eq.mp heq)
    simp [eq]
  · rfl

theorem ownedTool_lookup_member (world : World) (key : DocId) (tool : OwnedTool)
    (h : ownedToolByDocument? world key = some tool) :
    tool ∈ world.toolContexts ∧ tool.document = key := by
  unfold ownedToolByDocument? at h
  cases hf : world.toolContexts.filter (fun value => value.document == key) with
  | nil => simp [hf] at h
  | cons head tail =>
    cases tail with
    | cons next rest => simp [hf] at h
    | nil =>
      simp [hf] at h
      subst tool
      have hm : head ∈ world.toolContexts.filter (fun value => value.document == key) := by
        simp [hf]
      simpa using hm

end CanonicalOutput.Execution
