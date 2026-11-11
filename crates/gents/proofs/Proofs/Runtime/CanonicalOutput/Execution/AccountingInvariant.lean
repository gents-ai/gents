import Proofs.CanonicalOutput.Execution.AccountingPending
import Proofs.CanonicalOutput.Execution.AccountingRunning

namespace CanonicalOutput.Execution

theorem accountMetadataStep_preserves_toolProjectionCoherent
    (world : World) (generation : Generation) (tool : OwnedTool)
    (member : tool ∈ world.toolContexts) (coherent : toolProjectionCoherent world = true) :
    toolProjectionCoherent (accountMetadataStep generation world tool) = true := by
  by_cases pending : tool.context.state = .pending
  · exact accountMetadataStep_pending_preserves_toolProjectionCoherent world generation tool member pending coherent
  · by_cases running : tool.context.state = .running
    · exact accountMetadataStep_running_preserves_toolProjectionCoherent world generation tool member running coherent
    · rw [accountMetadataStep_terminal world generation tool member coherent ⟨pending, running⟩]
      exact coherent

private theorem accounting_retains_other (world : World) (generation : Generation)
    (selected other : OwnedTool) (member : other ∈ world.toolContexts)
    (different : other.document ≠ selected.document) :
    other ∈ (accountMetadataStep generation world selected).toolContexts := by
  unfold accountMetadataStep
  apply List.mem_map.mpr
  exact ⟨other, member, by simp [different]⟩

private theorem accounting_fold_coherent (todo : List OwnedTool) (world : World)
    (generation : Generation) (unique : (todo.map (·.document)).Nodup)
    (members : ∀ tool ∈ todo, tool ∈ world.toolContexts)
    (coherent : toolProjectionCoherent world = true) :
    toolProjectionCoherent (todo.foldl (accountMetadataStep generation) world) = true := by
  induction todo generalizing world with
  | nil => exact coherent
  | cons tool rest ih =>
    have step := accountMetadataStep_preserves_toolProjectionCoherent world generation tool
      (members tool (by simp)) coherent
    simp only [List.map_cons, List.nodup_cons] at unique
    apply ih _ unique.2 _ step
    intro other hm
    apply accounting_retains_other world generation tool other (members other (by simp [hm]))
    intro equal
    apply unique.1
    exact List.mem_map.mpr ⟨other, hm, equal⟩

theorem accountMetadataOwnedTools_preserves_toolProjectionCoherent
    (world : World) (generation : Generation) (coherent : toolProjectionCoherent world = true) :
    toolProjectionCoherent (accountMetadataOwnedTools world generation) = true := by
  have hc := toolProjectionCoherent_implies_lifecycle world coherent
  have unique := of_decide_eq_true (Bool.and_eq_true_iff.mp (Bool.and_eq_true_iff.mp hc).1).1
  exact accounting_fold_coherent world.toolContexts world generation unique (fun _ hm => hm) coherent

theorem revokeCorrupt_preserves_toolProjectionCoherent
    (before after : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (coherent : toolProjectionCoherent before = true)
    (h : revokeCorrupt before expected fresh outcome selection = .ok after) :
    toolProjectionCoherent after = true := by
  have hc := checked_core_success _ _ _ h
  unfold revokeCorruptCore at hc
  split at hc
  · cases hc; exact coherent
  · split at hc <;> try contradiction
    dsimp only at hc
    split at hc <;> try contradiction
    cases hc
    exact accountMetadataOwnedTools_preserves_toolProjectionCoherent before expected coherent

end CanonicalOutput.Execution
