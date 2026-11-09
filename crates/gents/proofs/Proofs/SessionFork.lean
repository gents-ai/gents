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

/-- One retained payload source, observed through its closing record. Payload
references name `logical`; a source's own closing record has `logical = id`. A
fork copy keeps the logical name its copied headers already carry while its
physical document (closing and source records) belongs to the copying session,
so header bytes and fork metadata stay exact and only the physical owner moves. -/
structure Closure where
  id : DocId
  session : SessionId
  logical : DocId
  deriving DecidableEq, Repr

structure History where
  messages : List MessageEnvelope
  compactions : List Compaction
  closures : List Closure := []
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

/-- Fork content is copied into destination-owned documents by `publish`;
retargeting a fork to another agent remains a separate, unadmitted decision. -/
def mayReuseSourceFragments (sourceAgent targetAgent : String) : Bool :=
  sourceAgent == targetAgent

theorem same_agent_may_reuse (agent : String) :
    mayReuseSourceFragments agent agent = true := by simp [mayReuseSourceFragments]

theorem cross_agent_cannot_reuse (source target : String) (different : source ≠ target) :
    mayReuseSourceFragments source target = false := by
  simp [mayReuseSourceFragments, beq_iff_eq, different]

def copyMessage (child : AgentSession.Scope) (childId : DocId)
    (childKey : String → String) (message : MessageEnvelope) : MessageEnvelope :=
  { message with
    header := forkHeader message.header childId child.session
    key := childKey message.key }

/-- The unique closure a reader of `session` resolves for a logical reference.
Resolution never leaves that session. -/
def resolveIn (closures : List Closure) (session : SessionId) (logical : DocId) :
    Option Closure :=
  match closures.filter (fun c => c.session == session && c.logical == logical) with
  | [closure] => some closure
  | _ => none

/-- The closure the source snapshot holds for a logical reference. An earlier
fork's headers resolve in that fork's own session; a source authored before
destination-owned copies may still name a physical closure elsewhere. -/
def sourceClosure (source : History) (logical : DocId) : Option Closure :=
  match source.closures.filter (·.logical == logical) with
  | [closure] => some closure
  | _ => none

def referencedLogicals (messages : List MessageEnvelope) : List DocId :=
  (messages.flatMap fun message => message.header.refs.map (·.closeId)).dedup

def retainedMessages (source : History) (cut : Nat) : List MessageEnvelope :=
  source.messages.filter (fun message => message.sequence < cut)

def copyPrefix (source : History) (child : AgentSession.Scope)
    (childDocumentId : DocId → DocId) (childKey : String → String)
    (cut : Nat) : History :=
  { messages := (retainedMessages source cut).map
      (fun message => copyMessage child (childDocumentId message.header.id) childKey message)
  , compactions := (source.compactions.filter (fun c => c.throughSequence < cut)).map
      (fun c => { c with id := childDocumentId c.id, session := child.session })
  , closures := (referencedLogicals (retainedMessages source cut)).filterMap fun logical =>
      (sourceClosure source logical).map fun closure =>
        { id := childDocumentId closure.id, session := child.session, logical } }

theorem copied_header_satisfies_reader_provenance (child : AgentSession.Scope)
    (id : DocId) (key : String → String) (origin : MessageEnvelope) :
    Hydration.forkMetadataMatches (copyMessage child id key origin) origin = true := by
  simp [Hydration.forkMetadataMatches, copyMessage, forkHeader]

theorem copied_prefix_preserves_order (source : History) (child : AgentSession.Scope)
    (ids : DocId → DocId) (keys : String → String) (cut : Nat) :
    ((copyPrefix source child ids keys cut).messages.map (·.sequence)) =
      (source.messages.filter (fun message => message.sequence < cut)).map (·.sequence) := by
  simp [copyPrefix, retainedMessages, copyMessage, List.map_map]

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
  history.messages.map (·.header.id) ++ history.compactions.map (·.id) ++
    history.closures.map (·.id)

/-- Every document a reader of `session` needs to reconstruct the transcript is
owned by that session: headers, compaction cursors and the closure each payload
reference resolves to there. Origin provenance is not consulted. -/
def selfContained (history : History) (session : SessionId) : Bool :=
  allInSession history session && history.closures.all (·.session == session) &&
    history.messages.all fun message => message.header.refs.all fun ref =>
      (resolveIn history.closures session ref.closeId).isSome

/-- Household ACP `children`: a session's documents are read through that
session's grant (`reader + working_agent + parent->read`). A direct session
reader holds exactly that grant and nothing on any other session. -/
def readableUnder (grant session : SessionId) : Bool := session == grant

def readerSessions (history : History) (session : SessionId) : List SessionId :=
  history.messages.map (·.header.session) ++ history.compactions.map (·.session) ++
    (history.messages.flatMap fun message => message.header.refs.filterMap fun ref =>
      (resolveIn history.closures session ref.closeId).map (·.session))

/-- A grant on `grant` reads the transcript of `session` exactly when every
payload reference resolves and every needed document is readable under it. -/
def readsTranscript (grant : SessionId) (history : History) (session : SessionId) : Bool :=
  (history.messages.all fun message => message.header.refs.all fun ref =>
      (resolveIn history.closures session ref.closeId).isSome) &&
    (readerSessions history session).all (readableUnder grant)

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
  closureDocuments : List DocId
  deriving DecidableEq, Repr

def authorizes (authorization : SourceAuthorization) (parent : AgentSession.Scope)
    (source : History) : Bool :=
  authorization.owner == parent &&
    source.messages.all (fun message => authorization.messageDocuments.contains message.header.id) &&
    source.compactions.all (fun compaction => authorization.compactionDocuments.contains compaction.id) &&
    source.closures.all (fun closure => authorization.closureDocuments.contains closure.id)

/-- Revision, authorization and source idleness remain inputs from existing
owners; allocators apply only to child messages/compactions/closure copies and
message keys. Logical payload references and tool identities are never
remapped; the referenced closures are copied into the child so the published
child is `selfContained`. -/
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
      selfContained copied child.session &&
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
    (copyMessage child childId childKey message).nativeRequestId = message.nativeRequestId ∧
    (copyMessage child childId childKey message).blocks = message.blocks ∧
    (copyMessage child childId childKey message).createdAt = message.createdAt ∧
    (copyMessage child childId childKey message).sequence = message.sequence := by
  exact ⟨rfl, rfl, rfl, rfl, rfl, rfl⟩

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
      copied.blocks = origin.blocks ∧ copied.nativeId = origin.nativeId ∧
      copied.nativeRequestId = origin.nativeRequestId := by
  simp only [copyPrefix, retainedMessages, List.mem_map, List.mem_filter] at h
  obtain ⟨origin, ⟨hmem, hcut⟩, rfl⟩ := h
  exact ⟨origin, hmem, of_decide_eq_true hcut,
    by simp [copyMessage, forkHeader], rfl, rfl, rfl, rfl⟩

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

theorem resolveIn_session {closures : List Closure} {session logical : DocId} {closure : Closure}
    (h : resolveIn closures session logical = some closure) : closure.session = session := by
  unfold resolveIn at h
  split at h
  · rename_i found hfilter
    simp only [Option.some.injEq] at h
    subst h
    have hmem : found ∈ closures.filter
        (fun c => c.session == session && c.logical == logical) := by
      rw [hfilter]; simp
    simp only [List.mem_filter, Bool.and_eq_true, beq_iff_eq] at hmem
    exact hmem.2.1
  · contradiction

theorem resolveIn_mem {closures : List Closure} {session logical : DocId} {closure : Closure}
    (h : resolveIn closures session logical = some closure) : closure ∈ closures := by
  unfold resolveIn at h
  split at h
  · rename_i found hfilter
    simp only [Option.some.injEq] at h
    subst h
    have hmem : found ∈ closures.filter
        (fun c => c.session == session && c.logical == logical) := by
      rw [hfilter]; simp
    exact (List.mem_filter.mp hmem).1
  · contradiction

theorem self_contained_reads_with_own_grant (history : History) (session : SessionId)
    (h : selfContained history session = true) :
    readsTranscript session history session = true := by
  simp only [selfContained, allInSession, Bool.and_eq_true, List.all_eq_true] at h
  obtain ⟨⟨⟨hmessages, hcompactions⟩, _⟩, hrefs⟩ := h
  simp only [readsTranscript, Bool.and_eq_true, List.all_eq_true]
  refine ⟨hrefs, ?_⟩
  intro observed hobserved
  simp only [readerSessions, List.mem_append, List.mem_map, List.mem_flatMap,
    List.mem_filterMap] at hobserved
  simp only [readableUnder, beq_iff_eq]
  rcases hobserved with (⟨message, hmessage, rfl⟩ | ⟨compaction, hcompaction, rfl⟩) |
      ⟨message, _, ref, _, hresolved⟩
  · simpa using hmessages message hmessage
  · simpa using hcompactions compaction hcompaction
  · simp only [Option.map_eq_some'] at hresolved
    obtain ⟨closure, hcase, rfl⟩ := hresolved
    exact resolveIn_session hcase

/-- The read-side fork contract: a published fork is self-contained, so a
direct reader granted only the child session reads its whole retained
transcript with no grant on, or visibility of, the source session. -/
theorem fork_reader_needs_only_child_grant
    (source : History) (parent child : AgentSession.Scope)
    (childDocumentId : DocId → DocId) (childKey : String → String) (cut : Nat)
    (authorization : SourceAuthorization) (idle coherent : Bool) (copied : History)
    (hpub : publish source parent child childDocumentId childKey cut
      authorization idle coherent = some copied) :
    selfContained copied child.session = true ∧
      readsTranscript child.session copied child.session = true := by
  have hcontained : selfContained copied child.session = true := by
    unfold publish at hpub
    dsimp only at hpub
    split at hpub
    · simp only [Option.some.injEq] at hpub
      subst copied
      rename_i hguard
      simp only [Bool.and_eq_true] at hguard
      exact hguard.1.2
    · contradiction
  exact ⟨hcontained, self_contained_reads_with_own_grant copied child.session hcontained⟩

/-- No grant on another session, including the source, substitutes for the
child grant once the child holds a message. -/
theorem other_grant_cannot_read_fork (history : History) (session grant : SessionId)
    (different : grant ≠ session) (message : MessageEnvelope)
    (hmessage : message ∈ history.messages) (hsession : message.header.session = session) :
    readsTranscript grant history session = false := by
  have hmem : session ∈ readerSessions history session := by
    simp only [readerSessions, List.mem_append, List.mem_map]
    exact Or.inl (Or.inl ⟨message, hmessage, hsession⟩)
  have hdenied : readableUnder grant session = false := by
    simp [readableUnder, beq_iff_eq, Ne.symm different]
  cases hread : readsTranscript grant history session with
  | false => rfl
  | true =>
      simp only [readsTranscript, Bool.and_eq_true, List.all_eq_true] at hread
      have := hread.2 session hmem
      simp [hdenied] at this

structure RetentionDependencies where
  originMessages : List DocId
  closingRecords : List DocId
  deriving DecidableEq, Repr

/-- Origin messages are provenance only. Closing records are the physical
closures this history's references resolve to in their own session, so a
fork depends on its destination-owned copies and never on source content. -/
def retentionDependencies (history : History) : RetentionDependencies :=
  { originMessages := (history.messages.filterMap (·.header.origin)).dedup
  , closingRecords := (history.messages.flatMap fun message =>
      message.header.refs.filterMap fun ref =>
        (resolveIn history.closures message.header.session ref.closeId).map (·.id)).dedup }

theorem closing_record_is_closure (history : History) (id : DocId)
    (h : id ∈ (retentionDependencies history).closingRecords) :
    id ∈ history.closures.map (·.id) := by
  simp only [retentionDependencies, List.mem_dedup, List.mem_flatMap,
    List.mem_filterMap] at h
  obtain ⟨message, _, ref, _, hresolved⟩ := h
  simp only [Option.map_eq_some'] at hresolved
  obtain ⟨closure, hcase, rfl⟩ := hresolved
  exact List.mem_map.mpr ⟨closure, resolveIn_mem hcase, rfl⟩

/-- Copied content is owned by the child: no closing record the published fork
depends on is a source document, so deleting source documents never removes one. -/
theorem source_deletion_preserves_fork_content
    (source : History) (parent child : AgentSession.Scope)
    (childDocumentId : DocId → DocId) (childKey : String → String) (cut : Nat)
    (authorization : SourceAuthorization) (idle coherent : Bool) (copied : History)
    (hpub : publish source parent child childDocumentId childKey cut
      authorization idle coherent = some copied)
    (id : DocId) (hsource : id ∈ documentIds source) :
    id ∉ (retentionDependencies copied).closingRecords := by
  obtain ⟨_, hfresh, _⟩ := publish_postconditions source parent child childDocumentId
    childKey cut authorization idle coherent copied hpub
  intro hdep
  have hmem := closing_record_is_closure copied id hdep
  have hcopied : id ∈ documentIds copied := by
    simp only [documentIds, List.mem_append]
    exact Or.inr hmem
  simp only [List.all_eq_true] at hfresh
  have := hfresh id hcopied
  simp [List.contains_eq_mem, hsource] at this

/-- The caller saves the child identity before requesting an edit fork. The
existing AgentSession owner persists its exact source-message binding with the
child, in the same transaction as prefix publication. This is a creation fact,
not an authorization grant or a second operation journal. -/
structure EditForkBinding where
  client : Nat
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
