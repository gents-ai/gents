import Proofs.CanonicalOutput.Execution.ToolMapFrame

namespace CanonicalOutput.Execution

/-- A terminal acknowledgement changes tool execution accounting, not already
published result authority. Lifecycle coherence is the close owner's existing
postcondition; the payload half follows from the prior coherent projection. -/
theorem terminalToolWrite_preserves_toolProjectionCoherent
    (world : World) (document : DocId) (state : ToolExecution.ToolCallState)
    (edit : OwnedTool → OwnedTool)
    (hdocument : ∀ tool, (edit tool).document = tool.document)
    (hrequest : ∀ tool, (edit tool).requestDoc = tool.requestDoc)
    (hsession : ∀ tool, (edit tool).session = tool.session)
    (hsequence : ∀ tool, (edit tool).acceptedSequence = tool.acceptedSequence)
    (hprovenance : ∀ tool, (edit tool).provenance = tool.provenance)
    (terminal : isTerminal state)
    (coherent : toolProjectionCoherent world = true)
    (lifecycle : toolLifecycleProjectionCoherent
      { world with
        toolContexts := world.toolContexts.map edit
        transcript := world.transcript.terminalizeToolCall document state } = true) :
    toolProjectionCoherent
      { world with
        toolContexts := world.toolContexts.map edit
        transcript := world.transcript.terminalizeToolCall document state } = true := by
  let post : World := { world with
    toolContexts := world.toolContexts.map edit
    transcript := world.transcript.terminalizeToolCall document state }
  let rowEdit := fun row : Transcript.ToolCallRow =>
    if row.callId = document then { row with state := state } else row
  have rowKey (row : Transcript.ToolCallRow) : (rowEdit row).callId = row.callId := by
    dsimp only [rowEdit]; split <;> rfl
  have lookupFrame (key : DocId) : transcriptToolByDocument? post key =
      (transcriptToolByDocument? world key).map rowEdit := by
    unfold transcriptToolByDocument?
    change (match (world.transcript.toolCalls.map rowEdit).filter (fun row => row.callId == key) with
      | [row] => some row | _ => none) = _
    rw [filter_map_key world.transcript.toolCalls (·.callId) key rowEdit rowKey]
    cases world.transcript.toolCalls.filter (fun row => row.callId == key) with
    | nil => rfl
    | cons head tail => cases tail <;> rfl
  have resultFrame (tool : OwnedTool) (key : Transcript.ToolResultKey) :
      canonicalToolResultBound post (edit tool) key = canonicalToolResultBound world tool key := by
    simp only [canonicalToolResultBound, post, Transcript.TranscriptState.terminalizeToolCall,
      hdocument, hrequest, hsession, hsequence, acceptedProviderId?]
  have receiptFrame (tool : OwnedTool) : runningReceiptSourceBound post (edit tool) =
      runningReceiptSourceBound world tool := by
    simp only [runningReceiptSourceBound, post, hdocument, hrequest, hsession]
  change toolProjectionCoherent post = true
  simp only [toolProjectionCoherent, Bool.and_eq_true] at coherent ⊢
  refine ⟨lifecycle, ?_⟩
  change (world.toolContexts.map edit).all _ = true
  rw [List.all_map, List.all_eq_true]
  intro tool hm
  have ht := (List.all_eq_true.mp coherent.2) tool hm
  simp only [Function.comp_def, hdocument, hprovenance, lookupFrame]
  cases hp : tool.provenance <;> cases hr : transcriptToolByDocument? world tool.document <;>
    simp only [hp, hr, Option.map] at ht ⊢
  all_goals try exact ht
  rename_i row
  have resultKey : (rowEdit row).resultKey = row.resultKey := by
    dsimp only [rowEdit]; split <;> rfl
  simp only [resultKey, resultFrame]
  cases hk : row.resultKey with
  | none => simp [hk]
  | some key =>
    simp only [hk, Bool.and_eq_true] at ht ⊢
    refine ⟨ht.1, ?_⟩
    by_cases selected : row.callId = document
    · simp [rowEdit, selected, terminal]
    · simpa only [rowEdit, selected, ↓reduceIte, receiptFrame] using ht.2

end CanonicalOutput.Execution
