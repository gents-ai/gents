import Proofs.AgentSession
import Proofs.CanonicalOutput.Message
import Proofs.CanonicalOutput.Hydration
import Mathlib.Data.List.Basic
import Mathlib.Data.List.Dedup

namespace SessionFork

open CanonicalOutput

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

def isUserTurn (message : MessageEnvelope) : Bool :=
  message.header.role == .user && message.blocks.any fun
    | .toolResult _ _ _ _ => false
    | _ => true

/-- Editors name the canonical message key rather than a position in a paged
transcript. The selected user turn and its suffix are excluded from the child;
missing, ambiguous, assistant, and tool-result-only anchors cannot select a cut.
Existing fork publication still owns source authorization, idleness, coherent
history, child identity, and origin retention. -/
def editCut (source : History) (messageKey : String) : Option Nat :=
  match source.messages.filter (·.key == messageKey) with
  | [message] => if isUserTurn message then some message.sequence else none
  | _ => none

theorem exact_edit_cut (source : History) (key : String) (message : MessageEnvelope)
    (unique : source.messages.filter (·.key == key) = [message])
    (user : isUserTurn message = true) :
    editCut source key = some message.sequence := by
  simp [editCut, unique, user]

theorem missing_edit_anchor (source : History) (key : String)
    (missing : source.messages.filter (·.key == key) = []) :
    editCut source key = none := by
  simp [editCut, missing]

theorem ambiguous_edit_anchor (source : History) (key : String)
    (first second : MessageEnvelope) (rest : List MessageEnvelope)
    (ambiguous : source.messages.filter (·.key == key) = first :: second :: rest) :
    editCut source key = none := by
  simp [editCut, ambiguous]

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

/-- Exact source snapshot grant from the existing document owner: the complete
parent scope is part of the grant and every copied header/compaction must be
named; a session-only or boolean grant is not accepted. -/
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
owners; allocators apply only to child messages/compactions and message keys —
payload, segment and tool identities are never remapped. -/
def publish (source : History) (parent child : AgentSession.Scope)
    (childDocumentId : DocId → DocId) (childKey : String → String) (cut : Nat)
    (authorization : SourceAuthorization) (idle coherent : Bool) : Option History :=
  let copied := copyPrefix source child childDocumentId childKey cut
  if authorizes authorization parent source && idle && coherent &&
      allInSession source parent.session &&
      decide (parent.node = child.node ∧ parent.requester = child.requester ∧
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

/-- The caller saves the child identity before requesting an edit fork. The
existing AgentSession owner persists its exact source-message binding with the
child, in the same transaction as prefix publication. This is a creation fact,
not an authorization grant or a second operation journal. -/
structure EditForkBinding where
  parent : AgentSession.Scope
  child : AgentSession.Scope
  messageKey : String
  deriving DecidableEq, Repr

def validEditForkBinding (binding : EditForkBinding) : Bool :=
  decide (binding.parent.node = binding.child.node ∧
    binding.parent.requester = binding.child.requester ∧
    binding.parent.session ≠ binding.child.session) && !binding.messageKey.isEmpty

inductive EditForkDecision where
  | create | reuse | refuse
  deriving DecidableEq, Repr

inductive ExistingEditFork where
  | missing | unbound | bound (binding : EditForkBinding)
  deriving DecidableEq, Repr

/-- The retained fact is read through the child's existing DID/ACP and exact
scope owner. Only `create` may proceed to `editCut` and `publish`, retaining all
their source authorization, coherence and idleness checks. Reuse does not read
today's source transcript or repeat prefix copying: it returns the previously
committed child. An unbound pre-existing child is never supplied as absence. -/
def decideEditFork (requested : EditForkBinding)
    (retained : ExistingEditFork) : EditForkDecision :=
  if validEditForkBinding requested then
    match retained with
    | .missing => .create
    | .unbound => .refuse
    | .bound committed => if committed = requested then .reuse else .refuse
  else .refuse

theorem exact_retained_edit_reuses_child (requested : EditForkBinding)
    (valid : validEditForkBinding requested = true) :
    decideEditFork requested (.bound requested) = .reuse := by
  simp [decideEditFork, valid]

theorem conflicting_edit_binding_is_refused (requested committed : EditForkBinding)
    (different : committed ≠ requested) :
    decideEditFork requested (.bound committed) = .refuse := by
  simp [decideEditFork, different]

theorem retained_edit_never_copies_again (requested committed : EditForkBinding) :
    decideEditFork requested (.bound committed) ≠ .create := by
  simp only [decideEditFork]
  split
  · split <;> simp
  · simp

theorem invalid_edit_scope_is_refused (requested : EditForkBinding)
    (retained : ExistingEditFork)
    (invalid : validEditForkBinding requested = false) :
    decideEditFork requested retained = .refuse := by
  simp [decideEditFork, invalid]

theorem unbound_child_is_not_replaced (requested : EditForkBinding) :
    decideEditFork requested .unbound = .refuse := by
  simp [decideEditFork]

end SessionFork
