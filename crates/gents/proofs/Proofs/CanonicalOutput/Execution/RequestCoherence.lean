import Proofs.CanonicalOutput.Execution.CoherenceFrame
import Proofs.CanonicalOutput.Execution.Properties

/-!
# Request-owned accounting frames

The fold step reuses the existing metadata accounting operation. Unowned and
already-terminal tools are unchanged; pending and running cases are proved in
their respective accounting modules and joined by `AccountingInvariant`.
-/
namespace CanonicalOutput.Execution

def accountMetadataStep (generation : Generation) (current : World)
    (original : OwnedTool) : World :=
  let (tool, transcript) := accountOneMetadataOwnedTool current generation original
  { current with
    toolContexts := replaceOwnedTool current.toolContexts original.document tool
    transcript := transcript }

private theorem replaceOwnedTool_self (tools : List OwnedTool) (tool : OwnedTool)
    (hnodup : (tools.map (fun value => value.document)).Nodup)
    (hmem : tool ∈ tools) :
    replaceOwnedTool tools tool.document tool = tools := by
  have same : tools.map (fun value => if value.document == tool.document then tool else value) =
      tools.map id := by
    apply List.map_congr_left
    intro value member
    split
    · rename_i key
      exact (List.inj_on_of_nodup_map hnodup member hmem (beq_iff_eq.mp key)).symm
    · rfl
  simpa only [replaceOwnedTool, List.map_id] using same

private theorem coherent_nonrunning_not_inFlight
    (world : World) (tool : OwnedTool)
    (hmem : tool ∈ world.toolContexts)
    (hstate : tool.context.state ≠ .running)
    (hcoherent : toolProjectionCoherent world = true) :
    tool.document ∉ world.transcript.inFlight := by
  have hlifecycle := toolProjectionCoherent_implies_lifecycle world hcoherent
  simp only [toolLifecycleProjectionCoherent, Bool.and_eq_true,
    List.all_eq_true] at hlifecycle
  have ht := hlifecycle.1.2 tool hmem
  rcases ht with ⟨_, ht⟩
  cases hp : tool.provenance with
  | acceptedIntent =>
      simp only [hp] at ht
      cases hr : transcriptToolByDocument? world tool.document <;>
        simp [hr, hstate] at ht
      have hin : decide (tool.document ∈ world.transcript.inFlight) = false := by
        have hsbool : (tool.context.state == .running) = false :=
          beq_eq_false_iff_ne.mpr hstate
        simpa [hsbool] using ht.2
      exact of_decide_eq_false hin
  | spawnedBackground parent =>
      simp only [hp, Bool.and_eq_true, Bool.not_eq_true] at ht
      exact of_decide_eq_true ht.2

private theorem releaseParentInFlight_eq_self
    (transcript : Transcript.TranscriptState) (document : DocId)
    (hnot : document ∉ transcript.inFlight) :
    transcript.releaseParentInFlight document = transcript := by
  unfold Transcript.TranscriptState.releaseParentInFlight
  simp [hnot]

theorem accountMetadataStep_unowned
    (world : World) (generation : Generation) (tool : OwnedTool)
    (hmem : tool ∈ world.toolContexts)
    (hnodup : (world.toolContexts.map (fun value => value.document)).Nodup)
    (hunowned : metadataOwnedByGeneration world generation tool = false) :
    accountMetadataStep generation world tool = world := by
  unfold accountMetadataStep accountOneMetadataOwnedTool
  simp only [hunowned, Bool.not_false, if_true]
  rw [replaceOwnedTool_self world.toolContexts tool hnodup hmem]

theorem accountMetadataStep_terminal
    (world : World) (generation : Generation) (tool : OwnedTool)
    (hmem : tool ∈ world.toolContexts)
    (hcoherent : toolProjectionCoherent world = true)
    (hterminal : tool.context.state ≠ .pending ∧ tool.context.state ≠ .running) :
    accountMetadataStep generation world tool = world := by
  have hlifecycle := toolProjectionCoherent_implies_lifecycle world hcoherent
  simp only [toolLifecycleProjectionCoherent, Bool.and_eq_true] at hlifecycle
  have hnodup : (world.toolContexts.map (fun value => value.document)).Nodup :=
    of_decide_eq_true hlifecycle.1.1
  have hnot := coherent_nonrunning_not_inFlight world tool hmem hterminal.2 hcoherent
  have hrelease := releaseParentInFlight_eq_self world.transcript tool.document hnot
  have hone : accountOneMetadataOwnedTool world generation tool = (tool, world.transcript) := by
    unfold accountOneMetadataOwnedTool
    split
    · rfl
    · cases hs : tool.context.state <;> simp_all
  unfold accountMetadataStep
  simp only [hone]
  rw [replaceOwnedTool_self world.toolContexts tool hnodup hmem]

end CanonicalOutput.Execution
