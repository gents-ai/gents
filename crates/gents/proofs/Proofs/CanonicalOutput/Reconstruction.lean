import Proofs.CanonicalOutput.State
import Mathlib.Data.List.Dedup

namespace CanonicalOutput

/-- Never select a first/last winner from conflicting visible documents. -/
private def uniqueRecordRaw {α ε : Type} (missing conflict : ε) : List α → Except ε α
  | [] => .error missing
  | [record] => .ok record
  | _ => .error conflict

private theorem uniqueRecordRaw_order_independent {α ε : Type} (missing conflict : ε)
    (left right : List α) (h : left.Perm right) :
    uniqueRecordRaw missing conflict left = uniqueRecordRaw missing conflict right := by
  cases left with
  | nil => rw [← h.nil_eq]
  | cons first rest =>
      cases rest with
      | nil => rw [← h.singleton_eq]
      | cons second tail =>
          have lengths := h.length_eq
          cases right with
          | nil => simp at lengths
          | cons r rs =>
              cases rs with
              | nil => simp at lengths
              | cons r' rs' => rfl

/-- Observations are sets of exact immutable facts. Duplicate delivery of the
same document is not a twin; a different identity or content remains a conflict. -/
def uniqueRecord {α ε : Type} [DecidableEq α] (missing conflict : ε)
    (records : List α) : Except ε α := uniqueRecordRaw missing conflict records.dedup

/-- Every visible document at this immutable identity has exactly these bytes. -/
def exactIdentityAt (records : List Segment) (record : Segment) : Bool :=
  (records.filter (fun other => other.id == record.id)).all (fun other => other == record)

/-- Flush timestamps respect ordinal order within a reconstructed source. -/
def timestampsNondecreasing (records : List Segment) : Bool :=
  records.all fun left => records.all fun right =>
    match left.flush, right.flush with
    | some a, some b => if a.ordinal ≤ b.ordinal then left.createdAt ≤ right.createdAt else true
    | _, _ => true

theorem uniqueRecord_order_independent {α ε : Type} [DecidableEq α]
    (missing conflict : ε) (left right : List α) (h : left.Perm right) :
    uniqueRecord missing conflict left = uniqueRecord missing conflict right :=
  uniqueRecordRaw_order_independent missing conflict _ _ h.dedup

theorem exact_duplicate_is_not_a_conflict {α ε : Type} [DecidableEq α]
    (missing conflict : ε) (record : α) :
    uniqueRecord missing conflict [record, record] = .ok record := by
  simp [uniqueRecord, uniqueRecordRaw]

theorem uniqueRecord_singleton {α ε : Type} [DecidableEq α]
    (missing conflict : ε) (record : α) :
    uniqueRecord missing conflict [record] = .ok record := by
  simp [uniqueRecord, uniqueRecordRaw]

inductive LookupError where
  | unavailable
  | denied
  | conflictingIdentity
  | invalidReference
  | conflictingClosures
  deriving DecidableEq, Repr

/-- `denied` is evidence from the authorization owner, never inferred from an
empty query. The record lookup intentionally distinguishes absence and denial. -/
def lookup (records : List Segment) (denied : List DocId) (id : DocId) :
    Except LookupError Segment :=
  if id ∈ denied then .error .denied
  else uniqueRecord .unavailable .conflictingIdentity
    (records.filter (fun r => r.id == id))

theorem lookup_order_independent (left right : List Segment) (denied : List DocId)
    (id : DocId) (h : left.Perm right) : lookup left denied id = lookup right denied id := by
  unfold lookup
  split
  · rfl
  · exact uniqueRecord_order_independent _ _ _ _ (h.filter _)

def resolveClose (records : List Segment) (denied : List DocId) (ref : PayloadRef) :
    Except LookupError Segment := do
  let record ← lookup records denied ref.closeId
  match record.close with
  | some (.closed _ _ streamBytes) =>
      if ref.stream < streamBytes.length then
        match uniqueRecord .conflictingClosures .conflictingClosures
            (closures records record.coordinate) with
        | .ok only => if only = record then .ok record else .error .conflictingClosures
        | .error error => .error error
      else .error .invalidReference
  | _ => .error .invalidReference

theorem resolveClose_order_independent (left right : List Segment) (denied : List DocId)
    (ref : PayloadRef) (h : left.Perm right) :
    resolveClose left denied ref = resolveClose right denied ref := by
  have choices (coordinate : Coordinate) :
      uniqueRecord LookupError.conflictingClosures .conflictingClosures (closures left coordinate) =
        uniqueRecord LookupError.conflictingClosures .conflictingClosures (closures right coordinate) :=
    uniqueRecord_order_independent _ _ _ _ ((h.filter _).filter _)
  simp only [resolveClose, lookup_order_independent left right denied ref.closeId h, choices]

theorem known_denial_is_not_loading (records : List Segment) (denied : List DocId)
    (id : DocId) (h : id ∈ denied) : lookup records denied id = .error .denied := by
  simp [lookup, h]

theorem absent_is_unavailable (id : DocId) :
    lookup [] [] id = .error .unavailable := rfl

theorem reference_denial_propagates (records : List Segment) (denied : List DocId)
    (ref : PayloadRef) (h : ref.closeId ∈ denied) :
    resolveClose records denied ref = .error .denied := by
  simp [resolveClose, lookup, h, Except.bind]
  rfl

inductive ExtentError where
  | missingOrdinal (ordinal : Nat)
  | conflictingOrdinal (ordinal : Nat)
  | invalidWriter
  | malformedRuns
  | extentMismatch
  | invalidClosure
  deriving DecidableEq, Repr

/-- Resolves one ordinal without selecting an arbitrary twin. -/
def flushAt (records : List Segment) (writer : Writer) (ordinal : Nat) :
    Except ExtentError Flush :=
  match uniqueRecord (.missingOrdinal ordinal) (.conflictingOrdinal ordinal)
      (records.filter (fun r => r.flush.any (fun f => f.ordinal == ordinal))) with
  | .error error => .error error
  | .ok record =>
      if record.writer ≠ writer then .error .invalidWriter
      else match record.flush with
      | some flush => .ok flush
      | none => .error .malformedRuns

theorem flushAt_order_independent (left right : List Segment) (writer : Writer)
    (ordinal : Nat) (h : left.Perm right) :
    flushAt left writer ordinal = flushAt right writer ordinal := by
  simp only [flushAt, uniqueRecord_order_independent _ _ _ _ (h.filter _)]

abbrev Streams := List (Declaration × List UInt8)

/-- Native declaration positions use lexicographic `(block, part)` order. Run
arrival need not follow this order. -/
def declarationBefore (left right : Declaration) : Bool :=
  left.block < right.block || (left.block == right.block && left.part < right.part)

def sameDeclarationPosition (left right : Declaration) : Bool :=
  left.block == right.block && left.part == right.part

def declarationPositionFresh (streams : Streams) (declaration : Declaration) : Bool :=
  streams.all fun existing => !sameDeclarationPosition existing.1 declaration

/-- Runs partition the payload at UTF-8 boundaries, declare streams densely once,
and never silently truncate an oversized run. Empty continuation writes are not
progress; only an opening declaration represents a meaningful empty string. -/
def consumeRuns : List Run → List UInt8 → Streams → Except ExtentError Streams
  | [], [], streams => .ok streams
  | [], _ :: _, _ => .error .malformedRuns
  | run :: rest, bytes, streams => do
      if run.bytes > bytes.length ∨ (run.bytes = 0 ∧ run.declaration.isNone) then
        .error .malformedRuns
      else
        let part := bytes.take run.bytes
        let remaining := bytes.drop run.bytes
        if (String.fromUTF8? (ByteArray.mk part.toArray)).isNone then .error .malformedRuns
        else match run.declaration with
        | some declaration =>
            if run.stream = streams.length ∧ declarationPositionFresh streams declaration ∧
                declarationWellFormed declaration then
              consumeRuns rest remaining (streams ++ [(declaration, part)])
            else .error .malformedRuns
        | none =>
            if run.stream < streams.length then
              consumeRuns rest remaining
                (streams.modify (fun old => (old.1, old.2 ++ part)) run.stream)
            else .error .malformedRuns

def consumeFlushes : List Flush → Streams → Except ExtentError Streams
  | [], streams => .ok streams
  | flush :: rest, streams => do
      if flush.runs.isEmpty then .error .malformedRuns
      else
        let next ← consumeRuns flush.runs flush.payload streams
        consumeFlushes rest next

/-- The coordinate's request is the already-authorized physical request boundary
supplied by the owner. This check never derives or broadens that scope; it only
checks the source/writer shape within it. Provider output is request-owned, tool
output is owned by that exact tool call, and authored output may use either
existing owner. -/
def writerMatchesSource (coordinate : Coordinate) (writer : Writer) : Bool :=
  match coordinate.source, writer with
  | .provider _ _ _, .request _ => true
  | .tool sourceCall, .tool writerCall => sourceCall == writerCall
  | .authored _, .request _ => true
  | .authored _, .tool _ => true
  | _, _ => false

/-- Called only after resolveClose has checked closure conflicts/authorization.
The full source is accounted for, but native JSON/media decoding is left to the
referencing header: omitted diagnostic streams need not decode as native blocks. -/
def reconstructExtent (records : List Segment) (closing : Segment) :
    Except ExtentError Streams := do
  if !writerMatchesSource closing.coordinate closing.writer then
    .error .invalidWriter
  else
    match closing.close with
    | some (.closed _ count expectedBytes) =>
        if closing.flush.any (fun f => f.ordinal + 1 != count) then
          .error .invalidClosure
        else
          let selected := extent records closing.coordinate count
          let flushes ← (List.range count).mapM (flushAt selected closing.writer)
          let streams ← consumeFlushes flushes []
          if streams.map (fun s => s.2.length) = expectedBytes then .ok streams
          else .error .extentMismatch
    | _ => .error .invalidClosure

theorem extent_order_independent (left right : List Segment) (closing : Segment)
    (h : left.Perm right) :
    reconstructExtent left closing = reconstructExtent right closing := by
  have slots (count ordinal : Nat) :
      flushAt (extent left closing.coordinate count) closing.writer ordinal =
        flushAt (extent right closing.coordinate count) closing.writer ordinal :=
    flushAt_order_independent _ _ _ _ ((h.filter _).filter _)
  have functions (count : Nat) :
      flushAt (extent left closing.coordinate count) closing.writer =
        flushAt (extent right closing.coordinate count) closing.writer :=
    funext (slots count)
  simp only [reconstructExtent, functions]

theorem reconstruction_ignores_beyond_extent (records : List Segment)
    (closing late : Segment) (outcome : Outcome) (count : Nat) (bytes : List Nat)
    (hclose : closing.close = some (.closed outcome count bytes))
    (hlate : inExtent count late = false) :
    reconstructExtent (records ++ [late]) closing = reconstructExtent records closing := by
  simp only [reconstructExtent, hclose, beyond_extent_inert records closing.coordinate count late hlate]

theorem no_flush_is_missing (writer : Writer) (ordinal : Nat) :
    flushAt [] writer ordinal = .error (.missingOrdinal ordinal) := rfl

theorem no_declaration_is_not_an_empty_stream (bytes : List UInt8) :
    consumeRuns [⟨0, bytes.length, none⟩] bytes [] = .error .malformedRuns := by
  simp [consumeRuns]

theorem explicit_empty_stream_is_preserved (declaration : Declaration)
    (h : declarationWellFormed declaration = true) :
    consumeRuns [⟨0, 0, some declaration⟩] [] [] = .ok [(declaration, [])] := by
  have hempty : String.fromUTF8? (ByteArray.mk (#[] : Array UInt8)) = some "" := by
    native_decide
  simp [consumeRuns, declarationPositionFresh, h, hempty]

inductive ReconstructionError where
  | lookup (error : LookupError)
  | extent (error : ExtentError)
  | missingStream
  deriving DecidableEq, Repr

/-- An existing hydration/authorization owner has established BOTH dependency
membership and denial. This is not inferred from a missing row. Root binding
lets a denial propagate even when the required segment/closure candidate is
absent locally; a bare denied document ID cannot establish that relationship. -/
structure DependencyDenial where
  rootCloseId : DocId
  deniedDocId : DocId
  deriving DecidableEq, Repr

/-- Shared composition: no consumer may skip closure resolution to read a shorter
prefix. Bare denied IDs cover known local rows; owner-verified dependency denials
cover required absent rows as well. Verifying that evidence belongs to native
hydration/ACP conformance. Native block decoding follows these bytes. -/
def reconstructPayload (records : List Segment) (denied : List DocId)
    (ref : PayloadRef) (dependencyDenials : List DependencyDenial := []) :
    Except ReconstructionError (Declaration × List UInt8) :=
  if dependencyDenials.any (fun d => d.rootCloseId == ref.closeId) then .error (.lookup .denied)
  else
  match resolveClose records denied ref with
  | .error error => .error (.lookup error)
  | .ok closing =>
      match closing.close with
      | some (.closed _ count _) =>
          if (extent records closing.coordinate count).any (fun r => r.id ∈ denied) then
            .error (.lookup .denied)
          else match reconstructExtent records closing with
          | .error error => .error (.extent error)
          | .ok streams => match streams[ref.stream]? with
            | none => .error .missingStream
            | some stream => .ok stream
      | _ => .error (.lookup .invalidReference)

theorem payload_denial_is_not_loading (records : List Segment) (denied : List DocId)
    (ref : PayloadRef) (h : ref.closeId ∈ denied) :
    reconstructPayload records denied ref = .error (.lookup .denied) := by
  simp [reconstructPayload, reference_denial_propagates records denied ref h]

theorem absent_dependency_denial_is_not_loading (records : List Segment)
    (denied : List DocId) (ref : PayloadRef) (dependency : DocId) :
    reconstructPayload records denied ref [⟨ref.closeId, dependency⟩] =
      .error (.lookup .denied) := by
  simp [reconstructPayload]

/-- Recovery inputs retain unique source stream indexes and unique native
declaration positions. Arrival order is deliberately irrelevant here. -/
def recoveryDeclarationsValid : List (Nat × Declaration) → Bool
  | [] => true
  | first :: rest =>
      !(rest.any fun other => first.1 == other.1 ||
        sameDeclarationPosition first.2 other.2) && recoveryDeclarationsValid rest

def insertRecoveryByPosition
    (entry : Nat × Declaration) : List (Nat × Declaration) → List (Nat × Declaration)
  | [] => [entry]
  | head :: tail =>
      if declarationBefore entry.2 head.2 then entry :: head :: tail
      else head :: insertRecoveryByPosition entry tail

def sortRecoveryByPosition : List (Nat × Declaration) → List (Nat × Declaration)
  | [] => []
  | head :: tail => insertRecoveryByPosition head (sortRecoveryByPosition tail)

theorem mem_insertRecoveryByPosition_iff (entry candidate : Nat × Declaration)
    (entries : List (Nat × Declaration)) :
    entry ∈ insertRecoveryByPosition candidate entries ↔
      entry = candidate ∨ entry ∈ entries := by
  induction entries with
  | nil => simp [insertRecoveryByPosition]
  | cons head tail ih =>
      simp only [insertRecoveryByPosition]
      split <;> simp [ih, or_assoc, or_left_comm]

theorem mem_sortRecoveryByPosition_iff (entry : Nat × Declaration)
    (entries : List (Nat × Declaration)) :
    entry ∈ sortRecoveryByPosition entries ↔ entry ∈ entries := by
  induction entries with
  | nil => simp [sortRecoveryByPosition]
  | cons head tail ih =>
      simp [sortRecoveryByPosition, mem_insertRecoveryByPosition_iff, ih]

/-- Recovery narrows declarations, not bytes. It never decodes incomplete tool
arguments or promotes opaque reasoning to text. Duplicate stream IDs or native
positions are rejected; surviving Text entries are sorted by native position
while retaining their source stream IDs. -/
def recoveryText (streams : List (Nat × Declaration)) :
    Except ExtentError (List (Nat × Declaration)) :=
  if recoveryDeclarationsValid streams then
    .ok (sortRecoveryByPosition (streams.filter (fun entry =>
      entry.2.kind == .text && entry.2.part == 0)))
  else .error .malformedRuns

theorem recovery_only_text (streams recovered : List (Nat × Declaration))
    (entry : Nat × Declaration) (hresult : recoveryText streams = .ok recovered)
    (h : entry ∈ recovered) :
    entry ∈ streams ∧ entry.2.kind = .text ∧ entry.2.part = 0 := by
  unfold recoveryText at hresult
  split at hresult
  · simp only [Except.ok.injEq] at hresult
    subst recovered
    rw [mem_sortRecoveryByPosition_iff] at h
    simpa using h
  · contradiction

theorem recovery_no_text_no_header (streams : List (Nat × Declaration))
    (hvalid : recoveryDeclarationsValid streams = true)
    (h : ∀ entry ∈ streams, entry.2.kind ≠ .text) :
    recoveryText streams = .ok [] := by
  simp only [recoveryText, hvalid, if_true, Except.ok.injEq, List.filter_eq_nil_iff]
  rw [show streams.filter (fun entry =>
      entry.2.kind == .text && entry.2.part == 0) = [] by
    apply List.filter_eq_nil_iff.mpr
    intro entry hentry
    simp [h entry hentry]]
  rfl

theorem recovery_rejects_duplicate_position
    (stream : Nat) (declaration : Declaration) :
    recoveryText [(stream, declaration), (stream + 1, { declaration with kind := .text })] =
      .error .malformedRuns := by
  simp [recoveryText, recoveryDeclarationsValid, sameDeclarationPosition]

inductive MessageRole where
  | system
  | user
  | assistant
  deriving DecidableEq, Repr

inductive MessagePublication where
  | requestExecution (generation : Nat)
  | requestRecovery (generation : Nat)
  | toolDelivery (call : DocId)
  | fork (origin : DocId)
  deriving DecidableEq, Repr

/-- Forking changes header ownership and publication provenance, not payload
references. This function is deliberately independent of runtime
execution/delivery state. -/
structure Header where
  id : DocId
  session : SessionId
  request : Option DocId
  origin : Option DocId
  refs : List PayloadRef
  outcome : Outcome
  role : MessageRole
  publication : MessagePublication
  deriving DecidableEq, Repr

def forkHeader (header : Header) (id : DocId) (session : SessionId) : Header :=
  { header with
    id := id
    session := session
    request := none
    origin := some header.id
    publication := .fork header.id }

theorem fork_preserves_references (header : Header) (id : DocId) (session : SessionId) :
    (forkHeader header id session).refs = header.refs ∧
    (forkHeader header id session).request = none ∧
    (forkHeader header id session).outcome = header.outcome ∧
    (forkHeader header id session).publication = .fork header.id := by
  exact ⟨rfl, rfl, rfl, rfl⟩

end CanonicalOutput
