import Proofs.Runtime.CanonicalOutput.Execution.Projection
import Proofs.Session.CanonicalOutput.ReconstructionFrame

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

theorem invocationName_append_open (world : World) (record : Segment)
    (_fresh : ∀ existing ∈ world.segments, existing.id ≠ record.id)
    (openSource : closures world.segments record.coordinate = [])
    (tool : OwnedTool) (name : String) (h : invocationName? world tool = some name) :
    invocationName? { world with segments := world.segments ++ [record] } tool = some name := by
  cases hp : tool.provenance with
  | acceptedIntent => simpa [invocationName?, hp] using h
  | spawnedBackground parent => simp [invocationName?, hp] at h
  | pluginEffect parent ordinal =>
    simp only [invocationName?, hp] at h ⊢
    cases ha : tool.effectArguments with
    | none => simp [ha] at h
    | some argument =>
      cases hf : world.segments.find? (fun segment => segment.id == argument) with
      | none => simp [ha, hf] at h
      | some closing =>
        have member : closing ∈ world.segments := List.mem_of_find?_eq_some hf
        have closed : closing.close.isSome = true := by
          cases hc : closing.close with
          | none =>
            cases hw : writerMatchesSource closing.coordinate closing.writer <;>
              simp [ha, hf, reconstructExtent, hc, hw] at h
          | some close => simp [hc]
        have differentCoordinate : record.coordinate ≠ closing.coordinate := by
          intro heq
          have hm : closing ∈ closures world.segments record.coordinate := by
            simp [closures, sourceRecords, member, closed, heq]
          simp [openSource] at hm
        have extents (count : Nat) :
            extent (world.segments ++ [record]) closing.coordinate count =
              extent world.segments closing.coordinate count := by
          simp [extent, sourceRecords, List.filter_append, differentCoordinate]
        simpa [ha, List.find?_append, hf, reconstructExtent, extents] using h

theorem invocationLinkValid_append_open (world : World) (record : Segment)
    (fresh : ∀ existing ∈ world.segments, existing.id ≠ record.id)
    (openSource : closures world.segments record.coordinate = [])
    (tool parent : OwnedTool) (h : invocationLinkValid world tool parent = true) :
    invocationLinkValid { world with segments := world.segments ++ [record] } tool parent = true := by
  cases hp : tool.provenance with
  | acceptedIntent => simp [invocationLinkValid, hp] at h
  | pluginEffect _ _ => simpa [invocationLinkValid, hp] using h
  | spawnedBackground _ =>
    simp only [invocationLinkValid, hp, Bool.and_eq_true, beq_iff_eq] at h ⊢
    exact ⟨h.1, h.2.1, invocationName_append_open world record fresh openSource parent _ h.2.2⟩

theorem invocationAncestryValid_append_open (world : World) (record : Segment)
    (fresh : ∀ existing ∈ world.segments, existing.id ≠ record.id)
    (openSource : closures world.segments record.coordinate = [])
    (fuel : Nat) (tool : OwnedTool) (h : invocationAncestryValid fuel world tool = true) :
    invocationAncestryValid fuel { world with segments := world.segments ++ [record] } tool = true := by
  induction fuel generalizing tool with
  | zero => simp [invocationAncestryValid, invocationAncestryWith] at h
  | succ fuel ih =>
    simp only [invocationAncestryValid, invocationAncestryWith] at h ⊢
    cases hp : invocationParent? tool with
    | none => simpa [hp, directAcceptedHeaderMetadataBindsTool] using h
    | some parentDoc =>
      have lookup : ownedToolByDocument? { world with segments := world.segments ++ [record] }
          parentDoc = ownedToolByDocument? world parentDoc := rfl
      simp only [hp] at h ⊢
      rw [lookup]
      cases hb : ownedToolByDocument? world parentDoc with
      | none => simp [hb] at h
      | some parent =>
        simp only [hb, Bool.and_eq_true] at h ⊢
        exact ⟨invocationLinkValid_append_open world record fresh openSource tool parent h.1,
          ih parent h.2⟩

theorem toolLifecycleProjectionCoherent_append_open (world : World) (record : Segment)
    (fresh : ∀ existing ∈ world.segments, existing.id ≠ record.id)
    (openSource : closures world.segments record.coordinate = [])
    (h : toolLifecycleProjectionCoherent world = true) :
    toolLifecycleProjectionCoherent { world with segments := world.segments ++ [record] } = true := by
  simp only [toolLifecycleProjectionCoherent, Bool.and_eq_true] at h ⊢
  refine ⟨⟨h.1.1, ?_⟩, h.2⟩
  rw [List.all_eq_true]
  intro tool member
  have ht := (List.all_eq_true.mp h.1.2) tool member
  simp only [Bool.and_eq_true] at ht ⊢
  exact ⟨⟨ht.1.1, invocationAncestryValid_append_open world record fresh openSource _ tool ht.1.2⟩, ht.2⟩

theorem toolProjectionCoherent_append_open (world : World) (record : Segment)
    (fresh : ∀ existing ∈ world.segments, existing.id ≠ record.id)
    (openSource : closures world.segments record.coordinate = [])
    (h : toolProjectionCoherent world = true) :
    toolProjectionCoherent { world with segments := world.segments ++ [record] } = true := by
  simp only [toolProjectionCoherent, Bool.and_eq_true] at h ⊢
  refine ⟨toolLifecycleProjectionCoherent_append_open world record fresh openSource h.1, ?_⟩
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
