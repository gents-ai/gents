import Proofs.CanonicalOutput.Execution.AccountingMetadata

namespace CanonicalOutput.Execution

def cancelPendingTool (document : DocId) (tool : OwnedTool) : OwnedTool :=
  if tool.document == document then { tool with context := { tool.context with state := .cancelled } }
  else tool

def cancelPendingRow (document : DocId) (row : Transcript.ToolCallRow) : Transcript.ToolCallRow :=
  if row.callId == document then { row with state := .cancelled } else row

def cancelPendingWorld (world : World) (document : DocId) : World :=
  { world with
    toolContexts := world.toolContexts.map (cancelPendingTool document)
    transcript := { world.transcript with
      toolCalls := world.transcript.toolCalls.map (cancelPendingRow document)
      inFlight := world.transcript.inFlight.erase document } }

theorem cancelPendingTool_document (document : DocId) (tool : OwnedTool) :
    (cancelPendingTool document tool).document = tool.document := by
  unfold cancelPendingTool; split <;> rfl

theorem cancelPendingRow_callId (document : DocId) (row : Transcript.ToolCallRow) :
    (cancelPendingRow document row).callId = row.callId := by
  unfold cancelPendingRow; split <;> rfl

theorem cancelPendingWorld_owned_lookup (world : World) (document key : DocId) :
    ownedToolByDocument? (cancelPendingWorld world document) key =
      (ownedToolByDocument? world key).map (cancelPendingTool document) := by
  unfold ownedToolByDocument? cancelPendingWorld
  rw [filter_map_key world.toolContexts (·.document) key (cancelPendingTool document)
    (cancelPendingTool_document document)]
  cases world.toolContexts.filter (fun tool => tool.document == key) with
  | nil => rfl
  | cons head tail => cases tail <;> rfl

theorem cancelPendingWorld_row_lookup (world : World) (document key : DocId) :
    transcriptToolByDocument? (cancelPendingWorld world document) key =
      (transcriptToolByDocument? world key).map (cancelPendingRow document) := by
  unfold transcriptToolByDocument? cancelPendingWorld
  rw [filter_map_key world.transcript.toolCalls (·.callId) key (cancelPendingRow document)
    (cancelPendingRow_callId document)]
  cases world.transcript.toolCalls.filter (fun row => row.callId == key) with
  | nil => rfl
  | cons head tail => cases tail <;> rfl

theorem cancelPendingWorld_result (world : World) (document : DocId) (tool : OwnedTool)
    (key : Transcript.ToolResultKey) :
    canonicalToolResultBound (cancelPendingWorld world document) (cancelPendingTool document tool) key =
      canonicalToolResultBound world tool key := by
  unfold cancelPendingTool
  split <;> rfl

theorem cancelPendingWorld_receipt (world : World) (document : DocId) (tool : OwnedTool) :
    runningReceiptSourceBound (cancelPendingWorld world document) (cancelPendingTool document tool) =
      runningReceiptSourceBound world tool := by
  unfold cancelPendingTool
  split <;> rfl

theorem transcriptTool_lookup_member (world : World) (key : DocId) (row : Transcript.ToolCallRow)
    (h : transcriptToolByDocument? world key = some row) :
    row ∈ world.transcript.toolCalls ∧ row.callId = key := by
  unfold transcriptToolByDocument? at h
  cases hf : world.transcript.toolCalls.filter (fun row => row.callId == key) with
  | nil => simp [hf] at h
  | cons head tail =>
    cases tail with
    | cons next rest => simp [hf] at h
    | nil =>
      simp [hf] at h
      subst row
      have hm : head ∈ world.transcript.toolCalls.filter (fun row => row.callId == key) := by simp [hf]
      simpa using hm

theorem cancelPendingWorld_binding (world : World) (document : DocId) (tool : OwnedTool) :
    acceptedHeaderBindsTool (cancelPendingWorld world document) (cancelPendingTool document tool) =
      acceptedHeaderBindsTool world tool := by
  apply acceptedHeaderBindsTool_map world (cancelPendingTool document) tool
  all_goals intro value; unfold cancelPendingTool; split <;> rfl

theorem cancelPendingWorld_lifecycle (world : World) (document : DocId)
    (pending : ∀ tool ∈ world.toolContexts, tool.document = document → tool.context.state = .pending)
    (coherent : toolLifecycleProjectionCoherent world = true) :
    toolLifecycleProjectionCoherent (cancelPendingWorld world document) = true := by
  simp only [toolLifecycleProjectionCoherent, Bool.and_eq_true] at coherent ⊢
  refine ⟨⟨?_, ?_⟩, ?_⟩
  · simpa only [cancelPendingWorld, List.map_map, Function.comp_def,
      cancelPendingTool_document] using coherent.1.1
  · change (world.toolContexts.map (cancelPendingTool document)).all _ = true
    rw [List.all_map, List.all_eq_true]
    intro tool hm
    have ht := (List.all_eq_true.mp coherent.1.2) tool hm
    simp only [Function.comp_def, cancelPendingTool_document, cancelPendingWorld_row_lookup,
      cancelPendingWorld_binding]
    have session : (cancelPendingTool document tool).session = tool.session := by
      unfold cancelPendingTool; split <;> rfl
    have provenance : (cancelPendingTool document tool).provenance = tool.provenance := by
      unfold cancelPendingTool; split <;> rfl
    simp only [session, provenance, Bool.and_eq_true] at ht ⊢
    refine ⟨ht.1, ?_⟩
    cases hp : tool.provenance with
    | acceptedIntent =>
      cases hr : transcriptToolByDocument? world tool.document with
      | none => simp [hp, hr] at ht
      | some row =>
        have key := (transcriptTool_lookup_member world tool.document row hr).2
        by_cases selected : tool.document = document
        · have hs := pending tool hm selected
          simp_all [hp, hr, cancelPendingTool, cancelPendingRow, cancelPendingWorld,
            toolHandedOff]
        · simp_all [hp, hr, cancelPendingTool, cancelPendingRow, cancelPendingWorld,
            toolHandedOff, Finset.mem_erase]
    | spawnedBackground parent =>
      by_cases selected : tool.document = document <;>
        simp_all [hp, cancelPendingTool, cancelPendingRow, cancelPendingWorld, Finset.mem_erase]
  · change (world.transcript.toolCalls.map (cancelPendingRow document)).all _ = true
    rw [List.all_map, List.all_eq_true]
    intro row hm
    have hr := (List.all_eq_true.mp coherent.2) row hm
    simp only [Function.comp_def, cancelPendingRow_callId, cancelPendingWorld_owned_lookup]
    cases hl : ownedToolByDocument? world row.callId with
    | none => simp [hl] at hr
    | some tool =>
      simp only [hl] at hr
      by_cases hs : tool.document = document <;> by_cases hrkey : row.callId = document <;>
        simpa [hl, hs, hrkey, cancelPendingTool, cancelPendingRow] using hr

theorem cancelPendingWorld_coherent (world : World) (document : DocId)
    (pending : ∀ tool ∈ world.toolContexts, tool.document = document → tool.context.state = .pending)
    (coherent : toolProjectionCoherent world = true) :
    toolProjectionCoherent (cancelPendingWorld world document) = true := by
  simp only [toolProjectionCoherent, Bool.and_eq_true] at coherent ⊢
  refine ⟨cancelPendingWorld_lifecycle world document pending coherent.1, ?_⟩
  change (world.toolContexts.map (cancelPendingTool document)).all _ = true
  rw [List.all_map, List.all_eq_true]
  intro tool hm
  have ht := (List.all_eq_true.mp coherent.2) tool hm
  have provenance : (cancelPendingTool document tool).provenance = tool.provenance := by
    unfold cancelPendingTool; split <;> rfl
  simp only [Function.comp_def, provenance, cancelPendingTool_document, cancelPendingWorld_row_lookup]
  cases hp : tool.provenance <;> cases hr : transcriptToolByDocument? world tool.document <;>
    simp only [hp, hr, Option.map] at ht ⊢
  all_goals try exact ht
  rename_i row
  have key := (transcriptTool_lookup_member world tool.document row hr).2
  have resultKey : (cancelPendingRow document row).resultKey = row.resultKey := by
    unfold cancelPendingRow; split <;> rfl
  simp only [resultKey, cancelPendingWorld_result]
  cases hk : row.resultKey with
  | none => simp [hk]
  | some result =>
    simp only [hk, Bool.and_eq_true] at ht ⊢
    refine ⟨ht.1, ?_⟩
    by_cases selected : tool.document = document
    · simp [cancelPendingRow, key, selected, isTerminal]
    · simpa only [cancelPendingRow, key, show (tool.document == document) = false by simpa using selected,
        Bool.false_eq_true, ↓reduceIte, cancelPendingWorld_receipt] using ht.2

theorem accountMetadataStep_pending_preserves_toolProjectionCoherent
    (world : World) (generation : Generation) (tool : OwnedTool)
    (member : tool ∈ world.toolContexts) (pending : tool.context.state = .pending)
    (coherent : toolProjectionCoherent world = true) :
    toolProjectionCoherent (accountMetadataStep generation world tool) = true := by
  have unique : (world.toolContexts.map (·.document)).Nodup := by
    have hc := toolProjectionCoherent_implies_lifecycle world coherent
    simpa only [toolLifecycleProjectionCoherent, Bool.and_eq_true, decide_eq_true_eq] using
      ((Bool.and_eq_true_iff.mp ((Bool.and_eq_true_iff.mp hc).1)).1)
  by_cases owned : metadataOwnedByGeneration world generation tool = true
  · have heffect : accountMetadataStep generation world tool = cancelPendingWorld world tool.document := by
      unfold accountMetadataStep accountOneMetadataOwnedTool
      simp only [owned, Bool.not_true, Bool.false_eq_true, ↓reduceIte, pending,
        ToolExecution.ToolCallContext.step?]
      rw [replaceOwnedTool_eq_map world tool
        (fun value => { value with context := { value.context with state := .cancelled } }) unique member]
      simp only [cancelPendingWorld, cancelPendingTool, cancelPendingRow,
        Transcript.TranscriptState.terminalizeToolCall, Transcript.TranscriptState.replaceToolCall,
        beq_iff_eq]
      have toolEdit : cancelPendingTool tool.document = fun value : OwnedTool =>
          if value.document = tool.document then
            { value with context := { value.context with state := .cancelled } } else value := by
        funext value; simp [cancelPendingTool]
      have rowEdit : cancelPendingRow tool.document = fun row : Transcript.ToolCallRow =>
          if row.callId = tool.document then { row with state := .cancelled } else row := by
        funext row; simp [cancelPendingRow]
      rw [toolEdit, rowEdit]
    rw [heffect]
    apply cancelPendingWorld_coherent world tool.document _ coherent
    intro other hmem heq
    have same := List.inj_on_of_nodup_map unique hmem member heq
    simpa [same] using pending
  · rw [accountMetadataStep_unowned world generation tool member unique (by simpa using owned)]
    exact coherent

end CanonicalOutput.Execution
