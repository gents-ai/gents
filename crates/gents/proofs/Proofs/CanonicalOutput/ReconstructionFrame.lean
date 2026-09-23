import Proofs.CanonicalOutput.Message
import Proofs.CanonicalOutput.ReconstructionGrowth

namespace CanonicalOutput

/-- Appending to an open source cannot modify the immutable closure selected
by an already successful reference. Global identity freshness is essential. -/
theorem resolveClose_append_open (records : List Segment) (record : Segment)
    (fresh : ∀ existing ∈ records, existing.id ≠ record.id)
    (openSource : closures records record.coordinate = [])
    (denied : List DocId) (ref : PayloadRef) (closing : Segment)
    (h : resolveClose records denied ref = .ok closing) :
    resolveClose (records ++ [record]) denied ref = .ok closing := by
  unfold resolveClose at h ⊢
  cases hl : lookup records denied ref.closeId with
  | error error => simp [hl, Bind.bind, Except.bind] at h
  | ok found =>
    have member : found ∈ records ∧ found.id = ref.closeId := by
      unfold lookup at hl
      split at hl <;> try contradiction
      have hm := uniqueRecord_success_mem _ _ _ _ hl
      simpa using hm
    have differentId : record.id ≠ ref.closeId := by
      intro heq
      exact fresh found member.1 (member.2.trans heq.symm)
    have lookupSame : lookup (records ++ [record]) denied ref.closeId = .ok found := by
      simpa [lookup, List.filter_append, differentId] using hl
    rw [lookupSame]
    simp only [hl, Bind.bind, Except.bind] at h ⊢
    cases hc : found.close with
    | none => simp [hc] at h
    | some close =>
      cases close with
      | retracted => simp [hc] at h
      | closed outcome count bytes =>
        have differentCoordinate : record.coordinate ≠ found.coordinate := by
          intro heq
          have hm : found ∈ closures records record.coordinate := by
            simp [closures, sourceRecords, member.1, heq, hc]
          simp [openSource] at hm
        have closuresSame : closures (records ++ [record]) found.coordinate =
            closures records found.coordinate := by
          simp [closures, sourceRecords, List.filter_append, differentCoordinate]
        simpa [hc, closuresSame] using h

theorem resolveClose_success_closed (records : List Segment) (denied : List DocId)
    (ref : PayloadRef) (closing : Segment)
    (h : resolveClose records denied ref = .ok closing) :
    closing ∈ records ∧ closing.close.isSome = true := by
  unfold resolveClose at h
  cases hl : lookup records denied ref.closeId with
  | error error => simp [hl, Bind.bind, Except.bind] at h
  | ok found =>
    have member : found ∈ records := by
      unfold lookup at hl
      split at hl <;> try contradiction
      have hm := uniqueRecord_success_mem _ _ _ _ hl
      exact (List.mem_filter.mp hm).1
    simp only [hl, Bind.bind, Except.bind] at h
    cases hc : found.close with
    | none => simp [hc] at h
    | some close =>
      cases close with
      | retracted => simp [hc] at h
      | closed outcome count bytes =>
        simp only [hc] at h
        repeat' split at h <;> try contradiction
        all_goals cases h; exact ⟨member, by simp [hc]⟩

theorem reconstructPayload_append_open (records : List Segment) (record : Segment)
    (fresh : ∀ existing ∈ records, existing.id ≠ record.id)
    (openSource : closures records record.coordinate = [])
    (denied : List DocId) (ref : PayloadRef) (payload : Declaration × List UInt8)
    (h : reconstructPayload records denied ref = .ok payload) :
    reconstructPayload (records ++ [record]) denied ref = .ok payload := by
  unfold reconstructPayload at h ⊢
  simp only [List.any_nil, Bool.false_eq_true, ↓reduceIte] at h ⊢
  cases hc : resolveClose records denied ref with
  | error error => simp [hc] at h
  | ok closing =>
    rw [resolveClose_append_open records record fresh openSource denied ref closing hc]
    simp only [hc] at h
    have member := resolveClose_success_closed records denied ref closing hc
    have differentCoordinate : record.coordinate ≠ closing.coordinate := by
      intro heq
      have hm : closing ∈ closures records record.coordinate := by
        simp [closures, sourceRecords, member.1, member.2, heq]
      simp [openSource] at hm
    have extents (count : Nat) : extent (records ++ [record]) closing.coordinate count =
        extent records closing.coordinate count := by
      simp [extent, sourceRecords, List.filter_append, differentCoordinate]
    simpa only [reconstructExtent, extents] using h

private def Preserves {ε α : Type} (before after : Except ε α) : Prop :=
  ∀ value, before = .ok value → after = .ok value

private theorem preserves_refl {ε α : Type} (value : Except ε α) :
    Preserves value value := fun _ h => h

private theorem preserves_bind {ε α β : Type} {before after : Except ε α}
    {f g : α → Except ε β} (first : Preserves before after)
    (rest : ∀ value, Preserves (f value) (g value)) :
    Preserves (before >>= f) (after >>= g) := by
  intro value h
  cases hb : before with
  | error error => simp [hb, Bind.bind, Except.bind] at h
  | ok item =>
    have ha := first item hb
    simp only [hb, Bind.bind, Except.bind] at h
    simpa only [ha, Bind.bind, Except.bind] using rest item value h

private theorem preserves_mapError {ε δ α : Type} {before after : Except ε α}
    (f : ε → δ) (h : Preserves before after) :
    Preserves (before.mapError f) (after.mapError f) := by
  intro value hv
  cases hb : before with
  | error error => simp [hb, Except.mapError] at hv
  | ok item =>
    simp [hb, Except.mapError] at hv
    subst value
    simp [h item hb, Except.mapError]

private theorem preserves_mapM {ε α β : Type} (items : List α)
    (f g : α → Except ε β) (h : ∀ item, Preserves (f item) (g item)) :
    Preserves (items.mapM f) (items.mapM g) := by
  induction items with
  | nil => exact preserves_refl _
  | cons item rest ih =>
    simp only [List.mapM_cons]
    apply preserves_bind (h item)
    intro value
    apply preserves_bind ih
    intro values
    exact preserves_refl _

private theorem preserves_forM {ε α : Type} (items : List α)
    (f g : α → Except ε PUnit) (h : ∀ item, Preserves (f item) (g item)) :
    Preserves (items.forM f) (items.forM g) := by
  induction items with
  | nil => exact preserves_refl _
  | cons item rest ih =>
    simp only [List.forM]
    exact preserves_bind (h item) (fun _ => ih)

section MessageFrame
variable (before after : List Segment) (denied : List DocId)
    (hc : ∀ ref, Preserves (resolveClose before denied ref) (resolveClose after denied ref))
    (hp : ∀ ref, Preserves (reconstructPayload before denied ref) (reconstructPayload after denied ref))
include hc hp

private theorem declaredPayload_frame (spec : PayloadSpec) :
    Preserves (declaredPayload before denied spec) (declaredPayload after denied spec) := by
  unfold declaredPayload
  apply preserves_bind (preserves_mapError _ (hc _))
  intro closing
  apply preserves_bind (preserves_mapError _ (hp _))
  intro payload
  exact preserves_refl _

omit hc in
private theorem resolveMessagePayload_frame (allowed : List PayloadKind) (json : Bool)
    (spec : PayloadSpec) : Preserves (resolveMessagePayload before denied allowed json spec)
      (resolveMessagePayload after denied allowed json spec) := by
  unfold resolveMessagePayload
  apply preserves_bind (preserves_mapError _ (hp _))
  intro payload
  exact preserves_refl _

omit hp in
private theorem validateReferenceSource_frame (header : Header) (ref : PayloadRef) :
    Preserves (validateReferenceSource before denied header ref)
      (validateReferenceSource after denied header ref) := by
  unfold validateReferenceSource
  exact preserves_bind (preserves_mapError _ (hc _)) (fun _ => preserves_refl _)

omit hp in
private theorem validateToolResultSource_frame (header : Header) (call : DocId) (ref : PayloadRef) :
    Preserves (validateToolResultSource before denied header call ref)
      (validateToolResultSource after denied header call ref) := by
  unfold validateToolResultSource
  exact preserves_bind (preserves_mapError _ (hc _)) (fun _ => preserves_refl _)

private theorem validateMediaDeclaration_frame (media : Media PayloadSpec) :
    Preserves (validateMediaDeclaration before denied media)
      (validateMediaDeclaration after denied media) := by
  unfold validateMediaDeclaration
  cases media.data <;> try exact preserves_refl _
  all_goals
    exact preserves_bind (declaredPayload_frame before after denied hc hp _)
      (fun _ => preserves_refl _)

private theorem validateBlockMetadata_frame (header : Header) (block : MessageBlock PayloadSpec) :
    Preserves (validateBlockMetadata before denied header block)
      (validateBlockMetadata after denied header block) := by
  cases block with
  | text | reasoning => exact preserves_refl _
  | toolCall doc id call name args sig extra =>
    exact preserves_bind (declaredPayload_frame before after denied hc hp _)
      (fun _ => preserves_refl _)
  | media media => exact validateMediaDeclaration_frame before after denied hc hp media
  | toolResult doc id call parts =>
    apply preserves_forM
    intro part
    cases part with
    | text p => exact validateToolResultSource_frame before after denied hc header doc _
    | media media =>
      apply preserves_bind (validateMediaDeclaration_frame before after denied hc hp media)
      intro _
      cases media.data <;> try exact preserves_refl _
      all_goals exact validateToolResultSource_frame before after denied hc header doc _

private theorem providerPositions_frame (refs : List PayloadRef) :
    Preserves (providerPositions before denied refs) (providerPositions after denied refs) := by
  unfold providerPositions
  apply preserves_bind
  · apply preserves_mapM
    intro ref
    apply preserves_bind (preserves_mapError _ (hc _))
    intro closing
    cases closing.coordinate.source <;> try exact preserves_refl _
    exact preserves_bind (preserves_mapError _ (hp _)) (fun _ => preserves_refl _)
  · intro _; exact preserves_refl _

private theorem validateProviderOrder_frame (message : MessageEnvelope) :
    Preserves (validateProviderOrder before denied message) (validateProviderOrder after denied message) := by
  unfold validateProviderOrder
  exact preserves_bind (providerPositions_frame before after denied hc hp _)
    (fun _ => preserves_refl _)

private theorem validateExactProviderPositions_frame (message : MessageEnvelope) :
    Preserves (validateExactProviderPositions before denied message)
      (validateExactProviderPositions after denied message) := by
  unfold validateExactProviderPositions
  apply preserves_forM
  intro expected
  apply preserves_bind (preserves_mapError _ (hc _))
  intro closing
  cases closing.coordinate.source <;> try exact preserves_refl _
  exact preserves_bind (preserves_mapError _ (hp _)) (fun _ => preserves_refl _)

private theorem validateRecoveryPositions_frame (message : MessageEnvelope) :
    Preserves (validateRecoveryPositions before denied message)
      (validateRecoveryPositions after denied message) := by
  unfold validateRecoveryPositions
  apply preserves_bind (validateProviderOrder_frame before after denied hc hp message)
  intro _
  apply preserves_forM
  intro ref
  apply preserves_bind (preserves_mapError _ (hc _))
  intro closing
  cases closing.coordinate.source <;> try exact preserves_refl _
  exact preserves_bind (preserves_mapError _ (hp _)) (fun _ => preserves_refl _)

private theorem validateMessageStructure_frame (message : MessageEnvelope) :
    Preserves (validateMessageStructure before denied message)
      (validateMessageStructure after denied message) := by
  unfold validateMessageStructure
  split
  · exact preserves_refl _
  · apply preserves_bind (preserves_forM _ _ _
      (validateReferenceSource_frame before after denied hc message.header))
    intro _
    apply preserves_bind (preserves_forM _ _ _
      (validateBlockMetadata_frame before after denied hc hp message.header))
    intro _
    cases message.header.publication with
    | requestExecution => exact validateExactProviderPositions_frame before after denied hc hp message
    | requestRecovery => exact validateRecoveryPositions_frame before after denied hc hp message
    | toolDelivery | fork => exact validateProviderOrder_frame before after denied hc hp message

omit hc in
private theorem reconstructMedia_frame (media : Media PayloadSpec) :
    Preserves (reconstructMedia (resolveMessagePayload before denied) media)
      (reconstructMedia (resolveMessagePayload after denied) media) := by
  unfold reconstructMedia
  cases media.data <;> try exact preserves_refl _
  all_goals
    exact preserves_bind (resolveMessagePayload_frame before after denied hp _ _ _)
      (fun _ => preserves_refl _)

omit hc in
private theorem reconstructBlock_frame (block : MessageBlock PayloadSpec) :
    Preserves (reconstructBlock (resolveMessagePayload before denied) block)
      (reconstructBlock (resolveMessagePayload after denied) block) := by
  cases block with
  | text p | toolCall doc id call name p sig extra =>
    exact preserves_bind (resolveMessagePayload_frame before after denied hp _ _ _)
      (fun _ => preserves_refl _)
  | reasoning id parts =>
    apply preserves_bind
    · apply preserves_mapM
      intro part
      cases part <;>
        exact preserves_bind (resolveMessagePayload_frame before after denied hp _ _ _)
          (fun _ => preserves_refl _)
    · intro _; exact preserves_refl _
  | toolResult doc id call parts =>
    apply preserves_bind
    · apply preserves_mapM
      intro part
      cases part with
      | text p =>
        exact preserves_bind (resolveMessagePayload_frame before after denied hp _ _ _)
          (fun _ => preserves_refl _)
      | media media =>
        exact preserves_bind (reconstructMedia_frame before after denied hp media)
          (fun _ => preserves_refl _)
    · intro _; exact preserves_refl _
  | media media =>
    exact preserves_bind (reconstructMedia_frame before after denied hp media)
      (fun _ => preserves_refl _)

private theorem reconstructMessage_frame (message : MessageEnvelope) :
    Preserves (reconstructMessage before denied message)
      (reconstructMessage after denied message) := by
  unfold reconstructMessage
  split
  · exact preserves_refl _
  · split
    · exact preserves_refl _
    · apply preserves_bind (validateMessageStructure_frame before after denied hc hp message)
      intro _
      apply preserves_bind (preserves_mapM _ _ _
        (reconstructBlock_frame before after denied hp))
      intro _
      exact preserves_refl _

end MessageFrame

/-- Successful native reconstruction is stable under writes to an open source.
This covers all native block kinds and metadata checks, not just text bytes. -/
theorem reconstructMessage_append_open (records : List Segment) (record : Segment)
    (fresh : ∀ existing ∈ records, existing.id ≠ record.id)
    (openSource : closures records record.coordinate = [])
    (denied : List DocId) (message : MessageEnvelope) (native : ReconstructedMessage)
    (h : reconstructMessage records denied message = .ok native) :
    reconstructMessage (records ++ [record]) denied message = .ok native :=
  reconstructMessage_frame records (records ++ [record]) denied
    (resolveClose_append_open records record fresh openSource denied)
    (reconstructPayload_append_open records record fresh openSource denied) message native h

end CanonicalOutput
