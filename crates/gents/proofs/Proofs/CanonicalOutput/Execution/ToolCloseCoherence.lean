import Proofs.CanonicalOutput.Execution.ToolDelivery
import Proofs.CanonicalOutput.Execution.ToolTerminalFrame

namespace CanonicalOutput.Execution.ToolDelivery

theorem terminalContext_isTerminal (world : World) (tool : OwnedTool)
    (authority : CloseAuthority) (context : ToolExecution.ToolCallContext)
    (h : terminalContext? world tool authority = some context) : isTerminal context.state := by
  cases authority <;> simp only [terminalContext?, Bind.bind, Option.bind] at h
  all_goals repeat' first | contradiction | (solve | simp_all) | (dsimp only at h) | split at h
  all_goals cases h; simpa using ‹decide (isTerminal _) = true›

theorem closeToolOutput_preserves_toolProjectionCoherent
    (before after : World) (document : DocId) (authority : CloseAuthority) (record : Segment)
    (coherent : toolProjectionCoherent before = true)
    (h : closeToolOutput before document authority record = .ok after) :
    toolProjectionCoherent after = true := by
  have lifecycle := (close_success_effect before after document authority record h).2
  rcases close_success_write_effect before after document authority record h with same | effect
  · simpa [same] using coherent
  · obtain ⟨tool, context, segments, found, terminal, closed, rfl⟩ := effect
    have member := ownedTool_lookup_member before document tool found
    have unique : (before.toolContexts.map (·.document)).Nodup := by
      have hc := toolProjectionCoherent_implies_lifecycle before coherent
      exact of_decide_eq_true (Bool.and_eq_true_iff.mp (Bool.and_eq_true_iff.mp hc).1).1
    let edit : OwnedTool → OwnedTool := fun value =>
      if value.document == document then clearReconcileIntent value context else value
    have mapEffect : replaceOwnedTool before.toolContexts document (clearReconcileIntent tool context) =
        before.toolContexts.map edit := by
      simpa only [edit, member.2] using
        replaceOwnedTool_eq_map before tool (fun value => clearReconcileIntent value context)
          unique member.1
    have segmentCoherent : toolProjectionCoherent { before with segments := segments } = true := by
      rcases CanonicalOutput.ToolDelivery.closeRecords_success_effect before.segments segments
        tool.requestDoc tool.document before.lease.now record closed with replay | fresh
      · simpa [replay] using coherent
      · obtain ⟨rfl, hopen, hfresh, howned⟩ := fresh
        apply toolProjectionCoherent_append_open before record _ _ coherent
        · simpa [CanonicalOutput.ToolDelivery.identityAvailable] using hfresh
        · have coordinate : record.coordinate = CanonicalOutput.ToolDelivery.coordinate tool.requestDoc tool.document := by
            simp only [CanonicalOutput.ToolDelivery.ownedRecord, Bool.and_eq_true] at howned
            exact beq_iff_eq.mp howned.1.1.1
          simpa [coordinate] using hopen
    rw [mapEffect] at lifecycle ⊢
    apply terminalToolWrite_preserves_toolProjectionCoherent
      { before with segments := segments } document context.state edit
    all_goals try (intro value; dsimp only [edit]; split <;> rfl)
    · exact terminalContext_isTerminal before tool authority context terminal
    · exact segmentCoherent
    · exact lifecycle

end CanonicalOutput.Execution.ToolDelivery
