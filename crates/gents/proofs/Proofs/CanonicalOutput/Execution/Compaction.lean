import Proofs.CanonicalOutput.Execution.State
import Proofs.Compaction.State

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
