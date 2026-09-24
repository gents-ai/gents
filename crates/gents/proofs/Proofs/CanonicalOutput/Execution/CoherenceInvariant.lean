import Proofs.CanonicalOutput.Execution.CoherenceFrame
import Proofs.CanonicalOutput.Execution.Properties
import Proofs.CanonicalOutput.Execution.Compaction

namespace CanonicalOutput.Execution

private theorem fresh_of_no_collision (world : World) (record : Segment)
    (collision : segmentIdentityCollision world record = false)
    (absent : record ∉ world.segments) :
    ∀ existing ∈ world.segments, existing.id ≠ record.id := by
  simp only [segmentIdentityCollision, List.any_eq_false] at collision
  intro existing hm heq
  have hc := collision existing hm
  simp [heq] at hc
  exact absent (hc ▸ hm)

private def authoredHeaderWorld (world : World) (message : MessageEnvelope) : World :=
  { world with
    messages := world.messages ++ [message]
    transcript := appendAuthoredRow world.transcript message }

private theorem authoredHeader_direct (world : World) (message : MessageEnvelope)
    (notAssistant : message.header.role ≠ .assistant) (tool : OwnedTool) :
    directAcceptedHeaderMetadataBindsTool (authoredHeaderWorld world message) tool =
      directAcceptedHeaderMetadataBindsTool world tool := by
  simp only [authoredHeaderWorld, directAcceptedHeaderMetadataBindsTool,
    List.filter_append, List.filter_singleton]
  simp only [show (message.header.role == MessageRole.assistant) = false from by simpa using notAssistant,
    Bool.and_false, Bool.false_and, Bool.false_eq_true, ↓reduceIte, List.append_nil]
  cases hr : message.header.role <;> simp only [hr, appendAuthoredRow]
  all_goals simp_all [Transcript.TranscriptState.appendUserMessage, List.any_append,
      Transcript.MessageKind.referencesToolCall]

private theorem authoredHeader_binding (world : World) (message : MessageEnvelope)
    (notAssistant : message.header.role ≠ .assistant) (tool : OwnedTool)
    (h : acceptedHeaderBindsTool world tool = true) :
    acceptedHeaderBindsTool (authoredHeaderWorld world message) tool = true := by
  unfold acceptedHeaderBindsTool at h ⊢
  cases hp : tool.provenance <;> simp only [hp] at h ⊢
  · rw [authoredHeader_direct world message notAssistant tool]; exact h
  · have lookupFrame : ∀ doc, ownedToolByDocument? (authoredHeaderWorld world message) doc =
        ownedToolByDocument? world doc := fun _ => rfl
    simp only [spawnParentIntentValid, lookupFrame] at h ⊢
    split at h <;> simp_all only [Bool.and_eq_true, Bool.false_eq_true, false_and]
    simp only [authoredHeader_direct world message notAssistant]
    simp only [authoredHeaderWorld, List.any_append, Bool.or_eq_true, Bool.and_eq_true] at h ⊢
    aesop

private theorem authoredHeader_rows (world : World) (message : MessageEnvelope) :
    (authoredHeaderWorld world message).transcript.toolCalls = world.transcript.toolCalls ∧
    (authoredHeaderWorld world message).transcript.inFlight = world.transcript.inFlight := by
  cases hr : message.header.role <;>
    simp [authoredHeaderWorld, appendAuthoredRow, hr, Transcript.TranscriptState.appendUserMessage]

private theorem authoredHeader_lifecycle (world : World) (message : MessageEnvelope)
    (notAssistant : message.header.role ≠ .assistant)
    (h : toolLifecycleProjectionCoherent world = true) :
    toolLifecycleProjectionCoherent (authoredHeaderWorld world message) = true := by
  have hf := authoredHeader_rows world message
  have lookupFrame : ∀ doc, transcriptToolByDocument? (authoredHeaderWorld world message) doc =
      transcriptToolByDocument? world doc := by
    intro doc; simp only [transcriptToolByDocument?, hf.1]
  simp only [toolLifecycleProjectionCoherent, Bool.and_eq_true] at h ⊢
  refine ⟨⟨h.1.1, ?_⟩, ?_⟩
  · simp only [List.all_eq_true] at h ⊢
    intro tool hm
    have ht := h.1.2 tool hm
    simp only [Bool.and_eq_true] at ht ⊢
    refine ⟨⟨ht.1.1, authoredHeader_binding world message notAssistant tool ht.1.2⟩, ?_⟩
    simpa only [lookupFrame, hf.2] using ht.2
  · simpa only [hf.1] using h.2

private theorem authoredHeader_result (world : World) (message : MessageEnvelope)
    (notAssistant : message.header.role ≠ .assistant) (generation : Generation)
    (publication : message.header.publication = .requestExecution generation)
    (tool : OwnedTool) (key : Transcript.ToolResultKey) :
    canonicalToolResultBound (authoredHeaderWorld world message) tool key =
      canonicalToolResultBound world tool key := by
  have hrole : (message.header.role == MessageRole.assistant) = false := by simpa using notAssistant
  cases hr : message.header.role <;>
    simp_all [canonicalToolResultBound, authoredHeaderWorld, appendAuthoredRow,
      Transcript.TranscriptState.appendUserMessage, acceptedProviderId?, List.filter_append,
      List.filter_singleton]

private theorem authoredHeader_receipt (world : World) (message : MessageEnvelope)
    (tool : OwnedTool) (h : runningReceiptSourceBound world tool = true) :
    runningReceiptSourceBound (authoredHeaderWorld world message) tool = true := by
  simp only [runningReceiptSourceBound, authoredHeaderWorld, List.any_append, Bool.or_eq_true] at h ⊢
  exact Or.inl h

private theorem authoredHeader_coherent (world : World) (message : MessageEnvelope)
    (notAssistant : message.header.role ≠ .assistant) (generation : Generation)
    (publication : message.header.publication = .requestExecution generation)
    (coherent : toolProjectionCoherent world = true) :
    toolProjectionCoherent (authoredHeaderWorld world message) = true := by
  simp only [toolProjectionCoherent, Bool.and_eq_true] at coherent ⊢
  refine ⟨authoredHeader_lifecycle world message notAssistant coherent.1, ?_⟩
  rw [List.all_eq_true] at coherent ⊢
  intro tool hm
  have ht := coherent.2 tool hm
  have rowFrame : transcriptToolByDocument? (authoredHeaderWorld world message) tool.document =
      transcriptToolByDocument? world tool.document := by
    simp only [transcriptToolByDocument?, (authoredHeader_rows world message).1]
  simp only [rowFrame]
  cases hp : tool.provenance <;>
    cases hr : transcriptToolByDocument? world tool.document <;>
    simp only [hp, hr] at ht ⊢
  all_goals try exact ht
  all_goals
    split at ht <;> try exact ht
    simp only [Bool.and_eq_true, Bool.or_eq_true,
      authoredHeader_result world message notAssistant generation publication] at ht ⊢
    refine ⟨ht.1, ?_⟩
    rcases ht.2 with terminal | ⟨running, receipt⟩
    · exact Or.inl terminal
    · exact Or.inr ⟨running, authoredHeader_receipt world message tool receipt⟩

theorem appendRaw_preserves_toolProjectionCoherent
    (before after : World) (generation : Generation) (record : Segment)
    (coherent : toolProjectionCoherent before = true)
    (h : appendRaw before generation record = .ok after) :
    toolProjectionCoherent after = true := by
  unfold appendRaw appendRawCore at h
  split at h <;> try contradiction
  rename_i hcollision
  split at h
  · split at h <;> try contradiction
    cases h
    exact coherent
  · rename_i habsent
    split at h <;> try contradiction
    rename_i hshape
    split at h <;> try contradiction
    dsimp only at h
    split at h <;> try contradiction
    cases h
    apply toolProjectionCoherent_append_open before record
      (fresh_of_no_collision before record (by simpa using hcollision) habsent) _ coherent
    have hopen : sourceOpen before record.coordinate = true := by
      simp_all
    simpa [sourceOpen] using hopen

theorem retractBeforeRetry_preserves_toolProjectionCoherent
    (before after : World) (generation : Generation) (record : Segment)
    (coherent : toolProjectionCoherent before = true)
    (h : retractBeforeRetry before generation record = .ok after) :
    toolProjectionCoherent after = true := by
  have hc := checked_core_success _ _ _ h
  unfold retractBeforeRetryCore at hc
  split at hc <;> try contradiction
  rename_i hcollision
  split at hc <;> try contradiction
  split at hc
  · split at hc <;> try contradiction
    cases hc
    exact coherent
  · rename_i habsent
    split at hc <;> try contradiction
    split at hc <;> try contradiction
    rename_i hopen
    split at hc <;> try contradiction
    cases hc
    apply toolProjectionCoherent_append_open before record
      (fresh_of_no_collision before record (by simpa using hcollision) habsent) _ coherent
    simpa [sourceOpen] using hopen

theorem publishAuthored_preserves_toolProjectionCoherent
    (before after : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope) (coherent : toolProjectionCoherent before = true)
    (h : publishAuthored before generation closing message = .ok after) :
    toolProjectionCoherent after = true := by
  have valid := (authored_publication_is_atomic_and_headed before after generation closing message h).2.2
  have notAssistant : message.header.role ≠ .assistant := by
    simp only [authoredMessageValid, Bool.and_eq_true] at valid
    simpa using valid.1.1.1.1.1.2
  have publication : message.header.publication = .requestExecution generation := by
    simp only [authoredMessageValid, Bool.and_eq_true] at valid
    simpa using valid.1.1.1.1.2
  have hc := checked_core_success _ _ _ h
  unfold publishAuthoredCore at hc
  split at hc <;> try contradiction
  split at hc
  · split at hc <;> try contradiction
    cases hc
    exact coherent
  · split at hc <;> try contradiction
    rename_i hfresh
    split at hc <;> try contradiction
    split at hc <;> try contradiction
    rename_i hopen
    split at hc <;> try contradiction
    dsimp only at hc
    split at hc <;> try contradiction
    split at hc <;> try contradiction
    cases hc
    have fresh : ∀ existing ∈ before.segments, existing.id ≠ closing.id := by
      simp only [Bool.or_eq_true, Bool.not_eq_false] at hfresh
      have hf : freshSegmentIdentity before closing = true := by simp_all
      simpa [freshSegmentIdentity] using hf
    have openSource : closures before.segments closing.coordinate = [] := by
      simpa [sourceOpen] using hopen
    exact authoredHeader_coherent { before with segments := before.segments ++ [closing] }
      message notAssistant generation publication
      (toolProjectionCoherent_append_open before closing fresh openSource coherent)

/-- Heartbeats update the lease only; tool facts retain their projection. -/
theorem renew_preserves_toolProjectionCoherent
    (before after : World) (generation : Generation) (deadline : Time)
    (coherent : toolProjectionCoherent before = true)
    (h : renew before generation deadline = .ok after) :
    toolProjectionCoherent after = true := by
  obtain ⟨lease, _, rfl⟩ := renew_success_is_exact_lease_cas before after generation deadline h
  exact coherent

theorem advanceCursor_preserves_toolProjectionCoherent
    (before after : World) (sequence : Transcript.Sequence)
    (coherent : toolProjectionCoherent before = true)
    (h : Compaction.advanceCursor? before sequence = some after) :
    toolProjectionCoherent after = true := by
  unfold Compaction.advanceCursor? at h
  repeat' split at h <;> try contradiction
  all_goals cases h; exact coherent

end CanonicalOutput.Execution
