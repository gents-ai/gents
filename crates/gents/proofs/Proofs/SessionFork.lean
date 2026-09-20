import Proofs.AgentSession
import Proofs.CanonicalOutput.Message
import Proofs.CanonicalOutput.Hydration
import Mathlib.Data.List.Basic
import Mathlib.Data.List.Dedup

/-!
# Header-only session forks

The source owner supplies one authorized coherent snapshot and an exclusive
message-sequence cut. A fork creates child `MessageEnvelope` rows so the native
blocks, native ID, references, outcome and creation provenance stay exact; only
the child header identity/session/publication, child-scoped key and request
membership change. It creates no payload, segment or tool-call row.
-/
namespace SessionFork

open CanonicalOutput

/-- Compaction entries remain child-session metadata. Their inclusive cursor
must name a retained child message sequence. -/
structure Compaction where
  id : DocId
  session : SessionId
  sequence : Nat
  throughSequence : Nat
  deriving DecidableEq, Repr

structure History where
  messages : List MessageEnvelope
  compactions : List Compaction
  deriving DecidableEq, Repr

def copyMessage (child : AgentSession.Scope) (childId : DocId)
    (childKey : String → String) (message : MessageEnvelope) : MessageEnvelope :=
  { message with
    header := forkHeader message.header childId child.session
    key := childKey message.key }

def copyPrefix (source : History) (child : AgentSession.Scope)
    (childDocumentId : DocId → DocId) (childKey : String → String)
    (cut : Nat) : History :=
  { messages := (source.messages.filter (fun message => message.sequence < cut)).map
      (fun message => copyMessage child (childDocumentId message.header.id) childKey message)
  , compactions := (source.compactions.filter (fun c => c.throughSequence < cut)).map
      (fun c => { c with id := childDocumentId c.id, session := child.session }) }

theorem copied_header_satisfies_reader_provenance (child : AgentSession.Scope)
    (id : DocId) (key : String → String) (origin : MessageEnvelope) :
    Hydration.forkMetadataMatches (copyMessage child id key origin) origin = true := by
  simp [Hydration.forkMetadataMatches, copyMessage, forkHeader]

/-- The publisher and reader use the same sequence space. No per-message
remapping can change tool-pair order or what an inherited compaction cursor names. -/
theorem copied_prefix_preserves_order (source : History) (child : AgentSession.Scope)
    (ids : DocId → DocId) (keys : String → String) (cut : Nat) :
    ((copyPrefix source child ids keys cut).messages.map (·.sequence)) =
      (source.messages.filter (fun message => message.sequence < cut)).map (·.sequence) := by
  simp [copyPrefix, copyMessage, List.map_map]

def validChain (session : SessionId) : Nat → Option Nat → List Compaction → Bool
  | _, _, [] => true
  | expected, previous, c :: rest => c.sequence == expected &&
      c.session == session && previous.all (· < c.throughSequence) &&
      validChain session (expected + 1) (some c.throughSequence) rest

def cursorPrefixesExist (history : History) : Bool :=
  history.compactions.all fun cursor =>
    history.messages.any (·.sequence == cursor.throughSequence)

def allInSession (history : History) (session : SessionId) : Bool :=
  history.messages.all (·.header.session == session) &&
    history.compactions.all (·.session == session)

def documentIds (history : History) : List DocId :=
  history.messages.map (·.header.id) ++ history.compactions.map (·.id)

def structurallyValid (history : History) : Bool :=
  decide (history.messages.map (·.sequence)).Nodup &&
    decide (history.messages.map (·.key)).Nodup &&
    decide (documentIds history).Nodup && cursorPrefixesExist history

/-- Exact source snapshot grant returned by the existing document owner.  The
complete parent scope is part of the grant and every copied header/compaction
must be named; a session-only or boolean grant is not accepted. -/
structure SourceAuthorization where
  owner : AgentSession.Scope
  messageDocuments : List DocId
  compactionDocuments : List DocId
  deriving DecidableEq, Repr

def authorizes (authorization : SourceAuthorization) (parent : AgentSession.Scope)
    (source : History) : Bool :=
  authorization.owner == parent &&
    source.messages.all (fun message => authorization.messageDocuments.contains message.header.id) &&
    source.compactions.all (fun compaction => authorization.compactionDocuments.contains compaction.id)

/-- Revision, authorization and source idleness remain inputs from existing
owners. Allocators apply only to child messages/compactions and message keys;
payload, segment and tool identities are never remapped. -/
def publish (source : History) (parent child : AgentSession.Scope)
    (childDocumentId : DocId → DocId) (childKey : String → String) (cut : Nat)
    (authorization : SourceAuthorization) (idle coherent : Bool) : Option History :=
  let copied := copyPrefix source child childDocumentId childKey cut
  if authorizes authorization parent source && idle && coherent &&
      allInSession source parent.session &&
      decide (parent.agent = child.agent ∧ parent.requester = child.requester ∧
        parent.session ≠ child.session) &&
      validChain parent.session 1 none source.compactions &&
      validChain child.session 1 none copied.compactions &&
      structurallyValid source && structurallyValid copied &&
      (documentIds copied).all (fun id => !(documentIds source).contains id) then
    some copied
  else none

theorem copy_message_detaches_live_membership (child : AgentSession.Scope)
    (childId : DocId) (childKey : String → String) (message : MessageEnvelope) :
    (copyMessage child childId childKey message).header.request = none ∧
    (copyMessage child childId childKey message).header.session = child.session ∧
    (copyMessage child childId childKey message).header.origin = some message.header.id ∧
    (copyMessage child childId childKey message).header.publication = .fork message.header.id := by
  simp [copyMessage, forkHeader]

theorem copy_message_preserves_native_content (child : AgentSession.Scope)
    (childId : DocId) (childKey : String → String) (message : MessageEnvelope) :
    (copyMessage child childId childKey message).header.refs = message.header.refs ∧
    (copyMessage child childId childKey message).nativeId = message.nativeId ∧
    (copyMessage child childId childKey message).blocks = message.blocks ∧
    (copyMessage child childId childKey message).createdAt = message.createdAt ∧
    (copyMessage child childId childKey message).sequence = message.sequence := by
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

theorem copy_message_rewrites_only_child_key (child : AgentSession.Scope)
    (childId : DocId) (childKey : String → String) (message : MessageEnvelope) :
    (copyMessage child childId childKey message).key = childKey message.key := rfl

theorem copied_message_carries_exact_origin
    (source : History) (child : AgentSession.Scope)
    (childDocumentId : DocId → DocId) (childKey : String → String)
    (cut : Nat) (copied : MessageEnvelope)
    (h : copied ∈ (copyPrefix source child childDocumentId childKey cut).messages) :
    ∃ origin ∈ source.messages,
      origin.sequence < cut ∧
      copied.header.origin = some origin.header.id ∧
      copied.header.refs = origin.header.refs ∧
      copied.blocks = origin.blocks ∧ copied.nativeId = origin.nativeId := by
  simp only [copyPrefix, List.mem_map, List.mem_filter] at h
  obtain ⟨origin, ⟨hmem, hcut⟩, rfl⟩ := h
  exact ⟨origin, hmem, of_decide_eq_true hcut,
    by simp [copyMessage, forkHeader], rfl, rfl, rfl⟩

theorem published_cursor_names_child_message
    (source : History) (parent child : AgentSession.Scope)
    (childDocumentId : DocId → DocId) (childKey : String → String) (cut : Nat)
    (authorization : SourceAuthorization) (idle coherent : Bool) (copied : History)
    (hpub : publish source parent child childDocumentId childKey cut
      authorization idle coherent = some copied)
    (cursor : Compaction) (hcursor : cursor ∈ copied.compactions) :
    ∃ message ∈ copied.messages, message.sequence = cursor.throughSequence := by
  have hvalid : cursorPrefixesExist copied = true := by
    unfold publish at hpub
    dsimp only at hpub
    split at hpub
    · simp only [Option.some.injEq] at hpub
      subst copied
      simp_all [structurallyValid]
    · contradiction
  simp only [cursorPrefixesExist, List.all_eq_true] at hvalid
  simpa only [List.any_eq_true, beq_iff_eq] using hvalid cursor hcursor

/-- Successful publication restores the durable postconditions: every output
row is in the child session, has a fresh identity, and retains its exact source
origin. -/
theorem publish_postconditions
    (source : History) (parent child : AgentSession.Scope)
    (childDocumentId : DocId → DocId) (childKey : String → String) (cut : Nat)
    (authorization : SourceAuthorization) (idle coherent : Bool) (copied : History)
    (hpub : publish source parent child childDocumentId childKey cut
      authorization idle coherent = some copied) :
    allInSession copied child.session = true ∧
    (documentIds copied).all (fun id => !(documentIds source).contains id) = true ∧
    ∀ message ∈ copied.messages, ∃ origin ∈ source.messages,
      message.header.origin = some origin.header.id := by
  unfold publish at hpub
  dsimp only at hpub
  split at hpub
  · simp only [Option.some.injEq] at hpub
    subst copied
    rename_i hguard
    simp only [Bool.and_eq_true] at hguard
    have hfresh : (documentIds (copyPrefix source child childDocumentId childKey cut)).all
        (fun id => !(documentIds source).contains id) = true := by
      aesop
    refine ⟨?_, hfresh, ?_⟩
    · simp [allInSession, copyPrefix, copyMessage, forkHeader]
    intro message hmessage
    obtain ⟨origin, horigin, _, hcopy, _, _, _⟩ :=
      copied_message_carries_exact_origin source child childDocumentId childKey cut message hmessage
    exact ⟨origin, horigin, hcopy⟩
  · contradiction

/-- Origin messages and closing records are retention dependencies, not copied
child payload. Exact extents and derived request/tool owners are added by
canonical hydration closure. -/
structure RetentionDependencies where
  originMessages : List DocId
  closingRecords : List DocId
  deriving DecidableEq, Repr

def retentionDependencies (history : History) : RetentionDependencies :=
  { originMessages := (history.messages.filterMap (·.header.origin)).dedup
  , closingRecords :=
      (history.messages.flatMap fun message => message.header.refs.map (·.closeId)).dedup }

def canDeleteOriginMessage (dependencies : RetentionDependencies) (id : DocId) : Bool :=
  !dependencies.originMessages.contains id

def canDeleteClosingRecord (dependencies : RetentionDependencies) (id : DocId) : Bool :=
  !dependencies.closingRecords.contains id

theorem copied_reference_is_retained (history : History) (message : MessageEnvelope)
    (ref : PayloadRef) (hmessage : message ∈ history.messages)
    (href : ref ∈ message.header.refs) :
    canDeleteClosingRecord (retentionDependencies history) ref.closeId = false := by
  have hmem : ref.closeId ∈
      (history.messages.flatMap fun message => message.header.refs.map (·.closeId)).dedup := by
    rw [List.mem_dedup]
    exact List.mem_flatMap.mpr
      ⟨message, hmessage, List.mem_map.mpr ⟨ref, href, rfl⟩⟩
  simpa [canDeleteClosingRecord, retentionDependencies, List.contains_eq_mem] using hmem

theorem copied_origin_message_is_retained (history : History) (message : MessageEnvelope)
    (origin : DocId) (hmessage : message ∈ history.messages)
    (horigin : message.header.origin = some origin) :
    canDeleteOriginMessage (retentionDependencies history) origin = false := by
  have hmem : origin ∈ (history.messages.filterMap (·.header.origin)).dedup := by
    rw [List.mem_dedup, List.mem_filterMap]
    exact ⟨message, hmessage, horigin⟩
  simpa [canDeleteOriginMessage, retentionDependencies, List.contains_eq_mem] using hmem

end SessionFork
