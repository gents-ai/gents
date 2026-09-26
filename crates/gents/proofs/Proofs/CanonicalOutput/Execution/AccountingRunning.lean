import Proofs.CanonicalOutput.Execution.AccountingMetadata

namespace CanonicalOutput.Execution

def handoffRunningEdit (world : World) (document : DocId) (tool : OwnedTool) : OwnedTool :=
  if tool.document == document then handoffRunningTool world tool else tool

def handoffRunningWorld (world : World) (document : DocId) : World :=
  { world with
    toolContexts := world.toolContexts.map (handoffRunningEdit world document)
    transcript := world.transcript.releaseParentInFlight document }

theorem handoffRunningEdit_document (world : World) (document : DocId) (tool : OwnedTool) :
    (handoffRunningEdit world document tool).document = tool.document := by
  unfold handoffRunningEdit handoffRunningTool; split <;> rfl

theorem handoffRunningEdit_requestDoc (world : World) (document : DocId) (tool : OwnedTool) :
    (handoffRunningEdit world document tool).requestDoc = tool.requestDoc := by
  unfold handoffRunningEdit handoffRunningTool; split <;> rfl

theorem handoffRunningEdit_session (world : World) (document : DocId) (tool : OwnedTool) :
    (handoffRunningEdit world document tool).session = tool.session := by
  unfold handoffRunningEdit handoffRunningTool; split <;> rfl

theorem handoffRunningEdit_sequence (world : World) (document : DocId) (tool : OwnedTool) :
    (handoffRunningEdit world document tool).acceptedSequence = tool.acceptedSequence := by
  unfold handoffRunningEdit handoffRunningTool; split <;> rfl

theorem handoffRunningEdit_provenance (world : World) (document : DocId) (tool : OwnedTool) :
    (handoffRunningEdit world document tool).provenance = tool.provenance := by
  unfold handoffRunningEdit handoffRunningTool; split <;> rfl

theorem handoffRunningEdit_await (world : World) (document : DocId) (tool : OwnedTool) :
    (handoffRunningEdit world document tool).context.awaitMode = tool.context.awaitMode := by
  unfold handoffRunningEdit handoffRunningTool; split <;> rfl

theorem handoffRunningEdit_state (world : World) (document : DocId) (tool : OwnedTool) :
    (handoffRunningEdit world document tool).context.state = tool.context.state := by
  unfold handoffRunningEdit handoffRunningTool; split <;> (try split) <;> rfl

theorem handoffRunningWorld_owned_lookup (world : World) (document key : DocId) :
    ownedToolByDocument? (handoffRunningWorld world document) key =
      (ownedToolByDocument? world key).map (handoffRunningEdit world document) := by
  unfold handoffRunningWorld
  exact ownedToolByDocument_map world (handoffRunningEdit world document) key
    (handoffRunningEdit_document world document)

theorem handoffRunningWorld_row_lookup (world : World) (document key : DocId) :
    transcriptToolByDocument? (handoffRunningWorld world document) key =
      transcriptToolByDocument? world key := by rfl

theorem handoffRunningWorld_header (world : World) (document : DocId) (tool : OwnedTool) :
    acceptedHeaderBindsTool (handoffRunningWorld world document)
      (handoffRunningEdit world document tool) = acceptedHeaderBindsTool world tool := by
  unfold handoffRunningWorld
  exact acceptedHeaderBindsTool_map world (handoffRunningEdit world document) tool
    (handoffRunningEdit_document world document) (handoffRunningEdit_requestDoc world document)
    (handoffRunningEdit_session world document) (handoffRunningEdit_sequence world document)
    (handoffRunningEdit_provenance world document)
    (handoffRunningEdit_await world document)

theorem handoffRunningWorld_result (world : World) (document : DocId) (tool : OwnedTool)
    (key : Transcript.ToolResultKey) :
    canonicalToolResultBound (handoffRunningWorld world document)
      (handoffRunningEdit world document tool) key = canonicalToolResultBound world tool key := by
  unfold handoffRunningWorld handoffRunningEdit handoffRunningTool; split <;> rfl

theorem handoffRunningWorld_receipt (world : World) (document : DocId) (tool : OwnedTool) :
    runningReceiptSourceBound (handoffRunningWorld world document)
      (handoffRunningEdit world document tool) = runningReceiptSourceBound world tool := by
  unfold handoffRunningWorld handoffRunningEdit handoffRunningTool; split <;> rfl

theorem accountMetadataStep_running_effect
    (world : World) (generation : Generation) (tool : OwnedTool)
    (member : tool ∈ world.toolContexts)
    (unique : (world.toolContexts.map (·.document)).Nodup)
    (owned : metadataOwnedByGeneration world generation tool = true)
    (running : tool.context.state = .running) :
    accountMetadataStep generation world tool = handoffRunningWorld world tool.document := by
  unfold accountMetadataStep accountOneMetadataOwnedTool
  simp only [owned, Bool.not_true, Bool.false_eq_true, ↓reduceIte, running]
  rw [replaceOwnedTool_eq_map world tool (handoffRunningTool world) unique member]
  unfold handoffRunningWorld handoffRunningEdit
  congr 1

theorem handoffRunningWorld_lifecycle (world : World) (document : DocId)
    (running : ∀ tool ∈ world.toolContexts, tool.document = document →
      tool.context.state = .running)
    (coherent : toolLifecycleProjectionCoherent world = true) :
    toolLifecycleProjectionCoherent (handoffRunningWorld world document) = true := by
  simp only [toolLifecycleProjectionCoherent, Bool.and_eq_true] at coherent ⊢
  refine ⟨⟨?_, ?_⟩, ?_⟩
  · simpa only [handoffRunningWorld, List.map_map, Function.comp_def,
      handoffRunningEdit_document] using coherent.1.1
  · change (world.toolContexts.map (handoffRunningEdit world document)).all _ = true
    rw [List.all_map, List.all_eq_true]
    intro tool hm
    have ht := (List.all_eq_true.mp coherent.1.2) tool hm
    simp only [Function.comp_def, handoffRunningEdit_document, handoffRunningEdit_session,
      handoffRunningEdit_provenance, handoffRunningEdit_sequence, handoffRunningEdit_state,
      handoffRunningWorld_row_lookup, handoffRunningWorld_header]
    simp only [Bool.and_eq_true] at ht ⊢
    refine ⟨ht.1, ?_⟩
    cases hp : tool.provenance with
    | acceptedIntent =>
      cases hr : transcriptToolByDocument? world tool.document with
      | none => simp [hp, hr] at ht
      | some row =>
        by_cases selected : tool.document = document
        · have hs := running tool hm selected
          simp_all [hp, hr, handoffRunningEdit, handoffRunningWorld,
            Transcript.TranscriptState.releaseParentInFlight, handoffRunningTool, toolHandedOff]
        · simpa [hp, hr, handoffRunningEdit, handoffRunningWorld,
            Transcript.TranscriptState.releaseParentInFlight, selected] using ht.2
    | spawnedBackground parent =>
      by_cases selected : tool.document = document <;>
        simp_all [hp, handoffRunningEdit, handoffRunningWorld,
          Transcript.TranscriptState.releaseParentInFlight]
  · change world.transcript.toolCalls.all _ = true
    rw [List.all_eq_true]
    intro row hm
    have hr := (List.all_eq_true.mp coherent.2) row hm
    rw [handoffRunningWorld_owned_lookup]
    cases hl : ownedToolByDocument? world row.callId with
    | none => simp [hl] at hr
    | some tool =>
      simp only [hl] at hr ⊢
      simpa [handoffRunningEdit_provenance, handoffRunningEdit_session,
        handoffRunningEdit_sequence] using hr

theorem handoffRunningWorld_coherent (world : World) (document : DocId)
    (running : ∀ tool ∈ world.toolContexts, tool.document = document →
      tool.context.state = .running)
    (coherent : toolProjectionCoherent world = true) :
    toolProjectionCoherent (handoffRunningWorld world document) = true := by
  simp only [toolProjectionCoherent, Bool.and_eq_true] at coherent ⊢
  refine ⟨handoffRunningWorld_lifecycle world document running coherent.1, ?_⟩
  change (world.toolContexts.map (handoffRunningEdit world document)).all _ = true
  rw [List.all_map, List.all_eq_true]
  intro tool hm
  have ht := (List.all_eq_true.mp coherent.2) tool hm
  simp only [Function.comp_def, handoffRunningEdit_document, handoffRunningEdit_provenance,
    handoffRunningWorld_row_lookup]
  cases hp : tool.provenance <;> cases hr : transcriptToolByDocument? world tool.document <;>
    simp only [hp, hr] at ht ⊢
  all_goals try exact ht
  rename_i row
  cases hk : row.resultKey with
  | none => simp [hk]
  | some key =>
    simp only [hk, Bool.and_eq_true] at ht ⊢
    exact ⟨by simpa [handoffRunningWorld_result] using ht.1,
      by simpa [handoffRunningWorld_receipt] using ht.2⟩

theorem accountMetadataStep_running_preserves_toolProjectionCoherent
    (world : World) (generation : Generation) (tool : OwnedTool)
    (member : tool ∈ world.toolContexts) (running : tool.context.state = .running)
    (coherent : toolProjectionCoherent world = true) :
    toolProjectionCoherent (accountMetadataStep generation world tool) = true := by
  have unique : (world.toolContexts.map (·.document)).Nodup := by
    have hc := toolProjectionCoherent_implies_lifecycle world coherent
    simpa only [toolLifecycleProjectionCoherent, Bool.and_eq_true, decide_eq_true_eq] using
      ((Bool.and_eq_true_iff.mp ((Bool.and_eq_true_iff.mp hc).1)).1)
  by_cases owned : metadataOwnedByGeneration world generation tool = true
  · rw [accountMetadataStep_running_effect world generation tool member unique owned running]
    apply handoffRunningWorld_coherent world tool.document _ coherent
    intro other hmem heq
    have same := List.inj_on_of_nodup_map unique hmem member heq
    simpa [same] using running
  · rw [accountMetadataStep_unowned world generation tool member unique (by simpa using owned)]
    exact coherent

end CanonicalOutput.Execution
