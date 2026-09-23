import Proofs.CanonicalOutput.Execution.State
import Proofs.Compaction.State
import Proofs.Compaction.ReductionEngine

/-!
# Canonical publication / provider compaction boundary

Execution rows use physical tool documents for ownership. The provider sanitizer
uses native call-id strings, which may be reused in later turns. This adapter
projects the reconstructed native blocks; it never casts physical document IDs
to provider symbols. Missing/conflicting payloads reject a projection instead of
silently shortening the history. System messages stay with the system-prompt
owner and are not user/assistant compaction rows.
-/
namespace CanonicalOutput.Execution.Compaction

open _root_.Compaction.PromptView

def nativeKind? (message : MessageEnvelope) (native : ReconstructedMessage) :
    Option Transcript.MessageKind :=
  let calls := nativeToolCallSymbols native
  let results := nativeToolResultFacts native
  match native.role with
  | .assistant =>
      if !results.isEmpty then none
      else if calls.isEmpty then some .ordinary
      else if calls.Nodup then some (.assistantToolCalls calls.toFinset)
      else none
  | .user =>
      if !calls.isEmpty then none
      else match native.blocks with
      | [.toolResult physical id callId _] =>
          if resultPublicationMatches message.header.publication physical then
            some (.toolResult (providerCallSymbol id callId)
              { sessionId := message.header.session
              , logicalResultId := physical, payloadHash := message.header.id })
          else none
      | _ => if results.isEmpty then some .ordinary else none
  | .system => none

def nativeRow? (message : MessageEnvelope) (native : ReconstructedMessage) :
    Option Transcript.MessageRow := do
  let kind ← nativeKind? message native
  let role ← match native.role with
    | .assistant => some Transcript.MessageRole.assistant
    | .user => some Transcript.MessageRole.user
    | .system => none
  pure { messageId := message.header.id, sessionId := message.header.session, sequence := message.sequence, role := role, kind := kind }

/-- Use the existing canonical display/reconstruction owner, including fork
origin validation. This is an authorized snapshot supplied by the native ACP
owner; it does not grant access to missing or denied dependencies. -/
def observation (world : World) (message : MessageEnvelope) : StreamingResponse.Observation :=
  { request := message.header.request.getD world.requestId
    session := world.sessionId
    records := world.segments, messages := world.messages
    deniedHeaders := [], deniedSegments := [], dependencyDenials := []
    owner := ⟨none, []⟩
    target := ⟨⟨message.header.request.getD world.requestId, .authored 0⟩,
      .request 0, some message.header.id⟩
    requestTerminal := false, terminalSelection := none }

def publishedRow? (world : World) (message : MessageEnvelope) :
    Option Transcript.MessageRow :=
  match StreamingResponse.project (observation world message) with
  | .published actual native =>
      if actual == message then nativeRow? message native else none
  | _ => none

/-! ## Native provider-projection seam

The provider serializer is deliberately not duplicated in Lean.  This adapter
does own the other half of the boundary: an explicitly selected, ordered list
of canonical message identities must resolve to the exact reconstructed native
messages before the native provider projection callback can run.  Missing,
conflicting, loading, or incomplete publications therefore fail the whole
request instead of being filtered out of the estimate.
-/

/-- Resolve one canonical identity to its exact published native message.
Duplicate delivery of the same immutable envelope is harmless; two different
envelopes at the identity are ambiguous and fail closed. -/
def reconstructedMessage? (world : World) (id : DocId) : Option ReconstructedMessage := do
  let message ← match (world.messages.filter fun candidate =>
      candidate.header.session == world.sessionId && candidate.header.id == id).dedup with
    | [message] => some message
    | _ => none
  match StreamingResponse.project (observation world message) with
  | .published actual native =>
      if actual == message then some native else none
  | _ => none

/-- Compose canonical reconstruction with the native provider owner. `fixed`
captures every non-message request layer. The error value describes failure at
this already-authorized snapshot boundary; authorization itself remains the
native observation owner's premise. -/
def canonicalProviderRequest {Fixed Request Error : Type}
    (world : World) (fixed : Fixed)
    (project : Fixed → List ReconstructedMessage → Except Error Request)
    (projectionError : Error) (messageIds : List DocId) : Except Error Request :=
  match messageIds.mapM (reconstructedMessage? world) with
  | none => .error projectionError
  | some native => project fixed native

theorem canonicalProviderRequest_success_uses_exact_reconstruction
    {Fixed Request Error : Type}
    (world : World) (messageIds : List DocId) (fixed : Fixed)
    (project : Fixed → List ReconstructedMessage → Except Error Request)
    (projectionError : Error) (request : Request)
    (h : canonicalProviderRequest world fixed project projectionError messageIds = .ok request) :
    ∃ native, messageIds.mapM (reconstructedMessage? world) = some native ∧
      project fixed native = .ok request := by
  cases hnatives : messageIds.mapM (reconstructedMessage? world) with
  | none => simp [canonicalProviderRequest, hnatives] at h
  | some native =>
      simp [canonicalProviderRequest, hnatives] at h
      exact ⟨native, rfl, h⟩

/-- Reconstruct the exact retained canonical suffix before injecting the new
checkpoint.  The checkpoint is a generated provider message payload, never
reinterpreted as a canonical document identity. -/
def canonicalRebuiltRequest {Fixed Request Error : Type}
    (world : World) (fixed : Fixed)
    (rebuild : Fixed → Nat → List ReconstructedMessage → Except Error Request)
    (projectionError : Error) (checkpoint : Nat) (retainedSuffix : List DocId) :
    Except Error Request :=
  match retainedSuffix.mapM (reconstructedMessage? world) with
  | none => .error projectionError
  | some native => rebuild fixed checkpoint native

/-- Canonical-output entry into the shared reduction/remeasurement owner.  The
initial projection and retained-suffix rebuild share the same fixed request
context, while only the latter receives the generated checkpoint. -/
def reduceCanonicalRebuildAndAuthorize {Fixed Request Error : Type}
    (world : World) (fixed : Fixed)
    (project : Fixed → List ReconstructedMessage → Except Error Request)
    (rebuild : Fixed → Nat → List ReconstructedMessage → Except Error Request)
    (estimate : Request → Except Error Nat) (projectionError : Error)
    (source : List DocId) (contextWindow thresholdBasisPoints configuredMaxOutputTokens : Nat)
    (canFit : Bool) (prefixLength checkpoint : Nat) :
    Except Error (_root_.Compaction.ReductionEngine.RebuiltDispatch Error Request) :=
  _root_.Compaction.ReductionEngine.reduceRebuildAndAuthorize
    (canonicalProviderRequest world fixed project projectionError)
    (canonicalRebuiltRequest world fixed rebuild projectionError)
    estimate source contextWindow thresholdBasisPoints configuredMaxOutputTokens
    canFit prefixLength checkpoint

theorem canonical_rebuilt_success_uses_exact_suffix {Fixed Request Error : Type}
    (world : World) (fixed : Fixed)
    (rebuild : Fixed → Nat → List ReconstructedMessage → Except Error Request)
    (projectionError : Error) (checkpoint : Nat) (retainedSuffix : List DocId)
    (request : Request)
    (h : canonicalRebuiltRequest world fixed rebuild projectionError checkpoint retainedSuffix =
      .ok request) :
    ∃ native, retainedSuffix.mapM (reconstructedMessage? world) = some native ∧
      rebuild fixed checkpoint native = .ok request := by
  cases hnatives : retainedSuffix.mapM (reconstructedMessage? world) with
  | none => simp [canonicalRebuiltRequest, hnatives] at h
  | some native =>
      simp [canonicalRebuiltRequest, hnatives] at h
      exact ⟨native, rfl, h⟩

/-- Canonical composition inherits the shared post-reduction threshold and
positive-output authorization; its projectors are definitionally the exact
canonical reconstruction adapters above. -/
theorem canonical_composed_dispatch_is_authorized {Fixed Request Error : Type}
    (world : World) (fixed : Fixed)
    (project : Fixed → List ReconstructedMessage → Except Error Request)
    (rebuild : Fixed → Nat → List ReconstructedMessage → Except Error Request)
    (estimate : Request → Except Error Nat) (projectionError : Error)
    (source : List DocId) (contextWindow thresholdBasisPoints configuredMaxOutputTokens : Nat)
    (canFit : Bool) (prefixLength checkpoint outputTokens : Nat)
    (projected : _root_.Compaction.ReductionEngine.RebuiltRequest Request)
    (h : reduceCanonicalRebuildAndAuthorize world fixed project rebuild estimate projectionError
      source contextWindow thresholdBasisPoints configuredMaxOutputTokens canFit prefixLength
      checkpoint = .ok (.dispatch projected outputTokens)) :
    let budget := PromptAssembly.Budget.effectiveInputBudget
      (PromptAssembly.Budget.configuredThresholdBudget contextWindow thresholdBasisPoints)
      contextWindow
    _root_.Compaction.ReductionEngine.decideThreshold projected.inputTokens budget = .notNeeded ∧
      PromptAssembly.Budget.CanDispatch projected.inputTokens contextWindow
        configuredMaxOutputTokens ∧ 0 < outputTokens ∧
      outputTokens = PromptAssembly.Budget.effectiveOutputBudget projected.inputTokens
        contextWindow configuredMaxOutputTokens ∧
      projected.inputTokens + outputTokens ≤ contextWindow := by
  exact _root_.Compaction.ReductionEngine.composed_dispatch_is_authorized
    (canonicalProviderRequest world fixed project projectionError)
    (canonicalRebuiltRequest world fixed rebuild projectionError)
    estimate source contextWindow thresholdBasisPoints configuredMaxOutputTokens canFit
    prefixLength checkpoint outputTokens projected h

/-- A successful composed dispatch used the exact reconstructed canonical
source and exact reconstructed retained suffix. The generated checkpoint is
passed separately to the rebuild owner. -/
theorem canonical_composed_dispatch_binds_reconstruction {Fixed Request Error : Type}
    (world : World) (fixed : Fixed)
    (project : Fixed → List ReconstructedMessage → Except Error Request)
    (rebuild : Fixed → Nat → List ReconstructedMessage → Except Error Request)
    (estimate : Request → Except Error Nat) (projectionError : Error)
    (source : List DocId) (contextWindow thresholdBasisPoints configuredMaxOutputTokens : Nat)
    (canFit : Bool) (prefixLength checkpoint outputTokens : Nat)
    (projected : _root_.Compaction.ReductionEngine.RebuiltRequest Request)
    (h : reduceCanonicalRebuildAndAuthorize world fixed project rebuild estimate projectionError
      source contextWindow thresholdBasisPoints configuredMaxOutputTokens canFit prefixLength
      checkpoint = .ok (.dispatch projected outputTokens)) :
    ∃ initialNative retainedNative initialRequest compactedPrefix retainedSuffix,
      source.mapM (reconstructedMessage? world) = some initialNative ∧
      project fixed initialNative = .ok initialRequest ∧
      compactedPrefix ++ retainedSuffix = source ∧
      retainedSuffix.mapM (reconstructedMessage? world) = some retainedNative ∧
      rebuild fixed checkpoint retainedNative = .ok projected.request := by
  obtain ⟨initial, compactedPrefix, retainedSuffix, _, hinitial, _, hpartition, _, _,
      hrebuilt, _⟩ :=
    _root_.Compaction.ReductionEngine.composed_dispatch_binds_exact_source
      (canonicalProviderRequest world fixed project projectionError)
      (canonicalRebuiltRequest world fixed rebuild projectionError) estimate source
      contextWindow thresholdBasisPoints configuredMaxOutputTokens canFit prefixLength checkpoint
      outputTokens projected h
  obtain ⟨initialNative, hsource, hproject⟩ :=
    canonicalProviderRequest_success_uses_exact_reconstruction world source fixed project
      projectionError initial.request hinitial
  obtain ⟨retainedNative, hsuffix, hrebuild⟩ :=
    canonical_rebuilt_success_uses_exact_suffix world fixed rebuild projectionError checkpoint
      retainedSuffix projected.request hrebuilt
  exact ⟨initialNative, retainedNative, initial.request, compactedPrefix, retainedSuffix, hsource,
    hproject, hpartition, hsuffix, hrebuild⟩

/-- A successful provider request witnesses the exact reconstructed list given
to the native projection owner; there is no independent list on which an
estimator could operate. -/
private theorem mapM_some_resolves_member {α β : Type} (f : α → Option β)
    (items : List α) (values : List β) (item : α)
    (hmapped : items.mapM f = some values) (hmember : item ∈ items) :
    ∃ value, f item = some value := by
  induction items generalizing values with
  | nil => simp at hmember
  | cons first rest ih =>
      cases hfirst : f first with
      | none => simp [hfirst] at hmapped
      | some firstValue =>
          cases hrest : rest.mapM f with
          | none => simp [hfirst, hrest] at hmapped
          | some restValues =>
              simp [hfirst, hrest] at hmapped
              subst values
              simp only [List.mem_cons] at hmember
              cases hmember with
              | inl heq => subst item; exact ⟨firstValue, hfirst⟩
              | inr hmember => exact ih restValues hrest hmember

/-- Any unresolved selected identity, including one in the middle or at the
end, prevents the callback from manufacturing a request from a shortened
history. -/
theorem canonicalProviderRequest_unresolved_selected_is_error
    {Fixed Request Error : Type}
    (world : World) (messageIds : List DocId) (id : DocId) (fixed : Fixed)
    (project : Fixed → List ReconstructedMessage → Except Error Request)
    (projectionError : Error)
    (hmember : id ∈ messageIds) (hmissing : reconstructedMessage? world id = none) :
    canonicalProviderRequest world fixed project projectionError messageIds =
      .error projectionError := by
  cases hnatives : messageIds.mapM (reconstructedMessage? world) with
  | none => simp [canonicalProviderRequest, hnatives]
  | some native =>
      obtain ⟨resolved, hresolved⟩ := mapM_some_resolves_member
        (reconstructedMessage? world) messageIds native id hnatives hmember
      rw [hmissing] at hresolved
      contradiction

/-- Empty, missing, ambiguous, and non-published observations cannot resolve a
native message merely because a provider callback exists. -/
theorem reconstructedMessage_requires_exact_publication
    (world : World) (id : DocId) (native : ReconstructedMessage)
    (h : reconstructedMessage? world id = some native) :
    ∃ message, (world.messages.filter fun candidate =>
        candidate.header.session == world.sessionId && candidate.header.id == id).dedup =
          [message] ∧
      StreamingResponse.project (observation world message) = .published message native := by
  let selected := (world.messages.filter fun candidate =>
    candidate.header.session == world.sessionId && candidate.header.id == id).dedup
  cases hselected : selected with
  | nil => simp [reconstructedMessage?, selected, hselected] at h
  | cons message rest =>
      cases rest with
      | cons second tail => simp [reconstructedMessage?, selected, hselected] at h
      | nil =>
          cases hproject : StreamingResponse.project (observation world message) with
          | published actual projected =>
              by_cases heq : actual = message
              · subst actual
                simp [reconstructedMessage?, selected, hselected, hproject] at h
                subst projected
                exact ⟨message, by simpa [selected] using hselected, hproject⟩
              · simp [reconstructedMessage?, selected, hselected, hproject, heq] at h
          | _ => simp [reconstructedMessage?, selected, hselected, hproject] at h

/-- Select an exact immutable prefix. The caller may choose a cursor but not
manufacture row classifications. Invalid/unsupported native shapes reject the
entire prefix. No unavailable message is filtered out as if it did not exist. -/
def prefixView? (world : World) (throughSequence : Transcript.Sequence) :
    Option _root_.Compaction.PromptView := do
  let messages := ((world.messages.filter fun message =>
    message.header.session == world.sessionId && message.sequence ≤ throughSequence &&
      message.header.role != .system).dedup).mergeSort (fun a b => a.sequence ≤ b.sequence)
  if !(messages.map (·.sequence)).Nodup then none
  if !(messages.map (·.header.id)).Nodup then none
  let rows ← messages.mapM (publishedRow? world)
  let view : _root_.Compaction.PromptView :=
    { sessionId := world.sessionId, messages := rows, summary := none
      outputObservations := fun id =>
        match messages.filter (fun message => message.header.id == id) with
        | [message] => some (observation world message)
        | _ => none }
  pure view

/-- Advance the already-compacted watermark only across an actually published,
provider-stable prefix in the shared allocator. This is eligibility/accounting
for the existing compaction owner, not a new summarizer. `prefixView?` may inspect
a future candidate; inspection alone never authorizes changing the watermark. -/
def advanceCursor? (world : World) (throughSequence : Transcript.Sequence) : Option World :=
  if throughSequence ≥ world.transcript.nextSeq then none
  else if world.compactionCursor.any (fun previous => throughSequence < previous) then none
  else match prefixView? world throughSequence with
    | none => none
    | some view =>
        if _root_.Compaction.PromptView.safeToReduce view then
          some { world with compactionCursor := some throughSequence }
        else none

theorem advanceCursor_preserves_publications
    (world after : World) (cursor : Transcript.Sequence)
    (h : advanceCursor? world cursor = some after) :
    after.transcript = world.transcript ∧ after.messages = world.messages ∧
      after.segments = world.segments ∧ after.compactionCursor = some cursor := by
  unfold advanceCursor? at h
  repeat' first | contradiction | (solve | cases h; exact ⟨rfl, rfl, rfl, rfl⟩) | split at h

theorem advanceCursor_requires_stable_prefix
    (world after : World) (cursor : Transcript.Sequence)
    (h : advanceCursor? world cursor = some after) :
    cursor < world.transcript.nextSeq ∧
      ∃ view, prefixView? world cursor = some view ∧
        _root_.Compaction.PromptView.safeToReduce view := by
  unfold advanceCursor? at h
  split at h
  · contradiction
  · rename_i hbound
    split at h
    · contradiction
    · split at h
      · contradiction
      · rename_i view hview
        split at h
        · rename_i hsafe
          exact ⟨Nat.lt_of_not_ge hbound, view, hview, hsafe⟩
        · contradiction

theorem advanceCursor_never_rewinds
    (world after : World) (previous cursor : Transcript.Sequence)
    (hprevious : world.compactionCursor = some previous)
    (h : advanceCursor? world cursor = some after) : previous ≤ cursor := by
  unfold advanceCursor? at h
  split at h
  · contradiction
  · split at h
    · contradiction
    · rename_i horder
      simp [hprevious] at horder
      exact horder

theorem native_result_uses_provider_key (message : MessageEnvelope)
    (physical : DocId) (id : String) (callId : Option String)
    (parts : List (ResultPart (List UInt8)))
    (hpublication : message.header.publication = .toolDelivery physical) :
    nativeKind? message ⟨.user, none, [.toolResult physical id callId parts]⟩ =
      some (.toolResult (providerCallSymbol id callId)
        ⟨message.header.session, physical, message.header.id⟩) := by
  simp [nativeKind?, nativeToolCallSymbols, nativeToolResultFacts,
    resultPublicationMatches, hpublication]

theorem ordinary_notification_is_not_a_native_result (message : MessageEnvelope)
    (bytes : List UInt8) :
    nativeKind? message ⟨.user, none, [.text bytes]⟩ = some .ordinary := by
  simp [nativeKind?, nativeToolCallSymbols, nativeToolResultFacts]

theorem native_row_preserves_canonical_identity
    (message : MessageEnvelope) (native : ReconstructedMessage)
    (row : Transcript.MessageRow) (h : nativeRow? message native = some row) :
    row.messageId = message.header.id ∧ row.sessionId = message.header.session ∧
      row.sequence = message.sequence := by
  cases hk : nativeKind? message native with
  | none => simp [nativeRow?, hk] at h
  | some kind =>
      cases hr : native.role <;> simp [nativeRow?, hk, hr] at h
      all_goals cases h; exact ⟨rfl, rfl, rfl⟩

theorem published_row_requires_actual_canonical_projection
    (world : World) (message : MessageEnvelope) (row : Transcript.MessageRow)
    (h : publishedRow? world message = some row) :
    ∃ native, StreamingResponse.project (observation world message) =
      .published message native ∧ nativeRow? message native = some row := by
  unfold publishedRow? at h
  split at h <;> try contradiction
  rename_i actual native hproject
  split at h
  · rename_i heq
    have heq' : actual = message := by simpa using heq
    subst actual
    exact ⟨native, hproject, h⟩
  · contradiction

/-- The compactor's provider-symbol projection never allocates or renumbers a
message. This connects its cursor space to the shared publication allocator. -/
theorem published_row_preserves_shared_sequence
    (world : World) (message : MessageEnvelope) (row : Transcript.MessageRow)
    (h : publishedRow? world message = some row) : row.sequence = message.sequence := by
  obtain ⟨native, _, hrow⟩ :=
    published_row_requires_actual_canonical_projection world message row h
  exact (native_row_preserves_canonical_identity message native row hrow).2.2

end CanonicalOutput.Execution.Compaction
