import Proofs.AgentSession
import Mathlib.Data.List.Basic

/-! The fork copier consumes one authorized transaction snapshot. Its ordinal cut
is resolved once from the user-turn API. Rows are selected by sequence/call identity,
never separate wall clocks. This models durable copy/link publication, not provider
sanitization or a new session execution framework. -/
namespace SessionFork

/-- Copy projection: for a call, `sequence` is resolved from its exact transcript
association; for compaction it is the persisted entry sequence. Never a new call field. -/
structure Row where
  /-- Interned (collection, _docID) reference, NOT a raw cross-collection _docID.
  Equal raw IDs in different collections have distinct tokens in this projection. -/
  docId : Nat
  scope : AgentSession.Scope
  sequence : Nat
  requestId : Option RequestId := none
  requestDocId : Option Nat := none
  spillRefs : List Nat := []
  deriving DecidableEq, Repr
structure Spill where
  row : Row
  callDocId : Nat
  deriving DecidableEq, Repr
structure Compaction where
  row : Row
  throughSequence : Nat
  keySession : SessionId
  deriving DecidableEq, Repr
structure History where
  messages : List Row
  calls : List Row
  spills : List Spill
  compactions : List Compaction
  deriving DecidableEq, Repr

/-- Map collection-qualified reference tokens to corresponding child tokens.
Injective assignment is local copy work, not durable metadata or a DB-wide
uniqueness requirement on raw _docID strings. -/
def copyRow (child : AgentSession.Scope) (ids : Nat → Nat) (row : Row) : Row :=
  { row with
    docId := ids row.docId
    scope := child
    requestId := none
    requestDocId := none
    spillRefs := row.spillRefs.map ids }

def copyPrefix (source : History) (child : AgentSession.Scope)
    (ids : Nat → Nat) (cut : Nat) : History :=
  let calls := source.calls.filter (fun row => row.sequence < cut)
  { messages := (source.messages.filter (fun row => row.sequence < cut)).map (copyRow child ids)
    calls := calls.map (copyRow child ids)
    spills := (source.spills.filter fun spill => calls.any (·.docId == spill.callDocId)).map
      (fun spill => { row := copyRow child ids spill.row, callDocId := ids spill.callDocId })
    compactions := (source.compactions.filter (fun c => c.throughSequence < cut)).map
      (fun c => { c with row := copyRow child ids c.row, keySession := child.session }) }

/-- Actual loader validates contiguous entry sequence, session chain key and
strictly increasing canonical cursors. Previous cursor is traversal state only. -/
def validChain (session : SessionId) : Nat → Option Nat → List Compaction → Bool
  | _, _, [] => true
  | expected, previous, c :: rest => c.row.sequence == expected &&
      c.keySession == session && previous.all (· < c.throughSequence) &&
      validChain session (expected + 1) (some c.throughSequence) rest

/-- Canonical cursor is inclusive and names an actual durable message sequence.
A count, out-of-range number, or missing source row cannot validate a copied
summary. Provider-space CursorDenotes remains the original compaction writer's
separate contract; this copy projection does not reconstruct provider messages. -/
def cursorPrefixesExist (h : History) : Bool :=
  h.compactions.all (fun c => h.messages.any (·.sequence == c.throughSequence))

def documentIds (h : History) : List Nat :=
  h.messages.map (·.docId) ++ h.calls.map (·.docId) ++
    h.spills.map (·.row.docId) ++ h.compactions.map (·.row.docId)

def allInScope (h : History) (scope : AgentSession.Scope) : Bool :=
  h.messages.all (·.scope == scope) && h.calls.all (·.scope == scope) &&
    h.spills.all (·.row.scope == scope) && h.compactions.all (·.row.scope == scope)

/-- Retained full-output references must resolve exactly, including references
inside copied truncated call output. A cut splitting a required link is rejected. -/
def linksResolve (h : History) : Bool :=
  (h.messages ++ h.calls).all (fun row => row.spillRefs.all
    (fun id => h.spills.any (fun spill => spill.row.docId == id))) &&
  h.spills.all (fun spill => h.calls.any (fun call => call.docId == spill.callDocId))

/-- The copy requires unique canonical message sequence keys and actual message
associations for calls. Snapshot-generation consistency alone does not prove either. -/
def structurallyValid (h : History) : Bool :=
  decide (h.messages.map (·.sequence)).Nodup &&
    h.calls.all (fun call => h.messages.any (·.sequence == call.sequence))

/-- Snapshot revision/busy/authorization are supplied by existing DB/source owners.
No partial child becomes visible: publication happens only after links/chain checks. -/
def publish (source : History) (parent child : AgentSession.Scope)
    (ids : Nat → Nat) (cut : Nat) (authorized idle coherent : Bool) : Option History :=
  let copied := copyPrefix source child ids cut
  if authorized && idle && coherent && allInScope source parent &&
      decide (parent.agent = child.agent ∧ parent.requester = child.requester ∧
        parent.session ≠ child.session) &&
      validChain parent.session 1 none source.compactions && validChain child.session 1 none copied.compactions &&
      linksResolve source && linksResolve copied &&
      cursorPrefixesExist source && cursorPrefixesExist copied &&
      structurallyValid source && structurallyValid copied &&
      (documentIds source).Nodup && (documentIds copied).Nodup &&
      (documentIds copied).all (fun id => !(documentIds source).contains id) then
    some copied
  else none

theorem copy_detaches (child : AgentSession.Scope) (ids : Nat → Nat) (row : Row) :
    (copyRow child ids row).scope = child ∧
    (copyRow child ids row).requestId = none ∧
    (copyRow child ids row).requestDocId = none := by simp [copyRow]

/-- Every retained spill points to a copied child call, independently of timestamps. -/
theorem copied_spill_resolves (source : History) (child : AgentSession.Scope)
    (ids : Nat → Nat) (cut : Nat) (spill : Spill)
    (hs : spill ∈ (copyPrefix source child ids cut).spills) :
    ∃ call ∈ (copyPrefix source child ids cut).calls, call.docId = spill.callDocId := by
  simp only [copyPrefix, List.mem_map, List.mem_filter] at hs
  obtain ⟨original, ⟨_, hcall⟩, rfl⟩ := hs
  simp only [List.any_eq_true, List.mem_filter, beq_iff_eq] at hcall
  obtain ⟨call, ⟨hc, hcut⟩, hid⟩ := hcall
  refine ⟨copyRow child ids call, ?_, ?_⟩
  · simp only [copyPrefix, List.mem_map, List.mem_filter]
    exact ⟨call, ⟨hc, hcut⟩, rfl⟩
  · simp [copyRow, hid]

theorem failed_copy_not_published (source : History) (parent child : AgentSession.Scope)
    (ids : Nat → Nat) (cut : Nat) (authorized idle : Bool) :
    publish source parent child ids cut authorized idle false = none := by
  simp [publish]

theorem published_chain_valid (source : History) (parent child : AgentSession.Scope)
    (ids : Nat → Nat) (cut : Nat) (authorized idle coherent : Bool) (copied : History)
    (h : publish source parent child ids cut authorized idle coherent = some copied) :
    validChain child.session 1 none copied.compactions = true := by
  unfold publish at h
  dsimp only at h
  split at h
  · simp only [Option.some.injEq] at h
    subst copied
    simp_all
  · contradiction

/-- Publication establishes a concrete retained canonical row for every cursor;
a numerically increasing but nonexistent prefix is insufficient. -/
theorem published_cursor_names_message (source : History) (parent child : AgentSession.Scope)
    (ids : Nat → Nat) (cut : Nat) (authorized idle coherent : Bool) (copied : History)
    (h : publish source parent child ids cut authorized idle coherent = some copied)
    (c : Compaction) (hc : c ∈ copied.compactions) :
    ∃ row ∈ copied.messages, row.sequence = c.throughSequence := by
  have hv : cursorPrefixesExist copied = true := by
    unfold publish at h
    dsimp only at h
    split at h
    · simp only [Option.some.injEq] at h
      subst copied
      simp_all
    · contradiction
  simp only [cursorPrefixesExist, List.all_eq_true] at hv
  simpa only [List.any_eq_true, beq_iff_eq] using hv c hc

theorem published_structure_valid (source : History) (parent child : AgentSession.Scope)
    (ids : Nat → Nat) (cut : Nat) (authorized idle coherent : Bool) (copied : History)
    (h : publish source parent child ids cut authorized idle coherent = some copied) :
    (copied.messages.map (·.sequence)).Nodup ∧
    ∀ call ∈ copied.calls, ∃ message ∈ copied.messages, message.sequence = call.sequence := by
  have hv : structurallyValid copied = true := by
    unfold publish at h
    dsimp only at h
    split at h
    · simp only [Option.some.injEq] at h
      subst copied
      simp_all
    · contradiction
  simpa only [structurallyValid, Bool.and_eq_true, decide_eq_true_eq,
    List.all_eq_true, List.any_eq_true, beq_iff_eq] using hv

end SessionFork
