import Proofs.CanonicalOutput.Execution.Projection
import Proofs.CanonicalOutput.ReconstructionFrame

namespace CanonicalOutput.Execution

theorem toolProjectionCoherent_implies_lifecycle (world : World)
    (h : toolProjectionCoherent world = true) :
    toolLifecycleProjectionCoherent world = true :=
  (Bool.and_eq_true_iff.mp h).1

theorem empty_toolProjectionCoherent (world : World)
    (htools : world.toolContexts = []) (hrows : world.transcript.toolCalls = []) :
    toolProjectionCoherent world = true := by
  simp [toolProjectionCoherent, toolLifecycleProjectionCoherent, htools, hrows]

theorem canonicalToolResultBound_append_open (world : World) (record : Segment)
    (fresh : ∀ existing ∈ world.segments, existing.id ≠ record.id)
    (openSource : closures world.segments record.coordinate = [])
    (tool : OwnedTool) (key : Transcript.ToolResultKey)
    (h : canonicalToolResultBound world tool key = true) :
    canonicalToolResultBound { world with segments := world.segments ++ [record] } tool key = true := by
  simp only [canonicalToolResultBound] at h ⊢
  split at h <;> simp_all only [Bool.and_eq_true, Bool.false_eq_true, and_false]
  split at h <;> simp_all only [Bool.and_eq_true, Bool.false_eq_true, and_false]
  rename_i _ message _
  cases hr : reconstructMessage world.segments noDeniedDocuments message with
  | error error => simp [hr] at h
  | ok native =>
    have hnew := reconstructMessage_append_open world.segments record fresh openSource
      noDeniedDocuments message native hr
    simp_all [Bool.and_eq_true, acceptedProviderId?]

theorem runningReceiptSourceBound_append_open (world : World) (record : Segment)
    (fresh : ∀ existing ∈ world.segments, existing.id ≠ record.id)
    (openSource : closures world.segments record.coordinate = [])
    (tool : OwnedTool) (h : runningReceiptSourceBound world tool = true) :
    runningReceiptSourceBound { world with segments := world.segments ++ [record] } tool = true := by
  simp only [runningReceiptSourceBound, List.any_eq_true, Bool.and_eq_true] at h ⊢
  obtain ⟨message, hm, ⟨⟨⟨⟨hpub, hrequest⟩, hsession⟩, hresult⟩, hrefs⟩⟩ := h
  refine ⟨message, hm, ⟨⟨⟨⟨hpub, hrequest⟩, hsession⟩, hresult⟩, ?_⟩⟩
  rw [List.all_eq_true] at hrefs ⊢
  intro ref hmem
  have hp := hrefs ref hmem
  cases hr : resolveClose world.segments noDeniedDocuments ref with
  | error error => simp [hr] at hp
  | ok closing =>
    rw [resolveClose_append_open world.segments record fresh openSource
      noDeniedDocuments ref closing hr]
    simpa [hr] using hp

theorem toolProjectionCoherent_append_open (world : World) (record : Segment)
    (fresh : ∀ existing ∈ world.segments, existing.id ≠ record.id)
    (openSource : closures world.segments record.coordinate = [])
    (h : toolProjectionCoherent world = true) :
    toolProjectionCoherent { world with segments := world.segments ++ [record] } = true := by
  simp only [toolProjectionCoherent, Bool.and_eq_true] at h ⊢
  refine ⟨h.1, ?_⟩
  rw [List.all_eq_true] at h ⊢
  intro tool hmem
  have ht := h.2 tool hmem
  have rowFrame : transcriptToolByDocument? { world with segments := world.segments ++ [record] }
      tool.document = transcriptToolByDocument? world tool.document := rfl
  simp only [rowFrame]
  cases hp : tool.provenance <;>
    cases hr : transcriptToolByDocument? world tool.document <;>
    simp only [hp, hr] at ht ⊢
  all_goals try exact ht
  all_goals
    split at ht <;> try exact ht
    simp only [Bool.and_eq_true, Bool.or_eq_true] at ht ⊢
    refine ⟨canonicalToolResultBound_append_open world record fresh openSource tool _ ht.1, ?_⟩
    rcases ht.2 with terminal | ⟨running, receipt⟩
    · exact Or.inl terminal
    · exact Or.inr ⟨running, runningReceiptSourceBound_append_open world record fresh openSource tool receipt⟩

end CanonicalOutput.Execution
