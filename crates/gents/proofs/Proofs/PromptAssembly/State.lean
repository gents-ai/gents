import Proofs.Basic
import Proofs.Transcript.State

/-!
# PromptAssembly State (issue #448)

Vocabulary for the provider-input narrowing model: the durable transcript is
*permissive* (`Transcript.PairClosed` only constrains completed calls), while
providers reject any assistant tool-call not followed immediately by its
result block and any tool-result not closing the active call block. `sanitize` (Rust:
`compaction::sanitize_history_for_provider`, applied at the
`run_loop_stream` entry chokepoint) narrows the former to the latter.

Granularity note: a `Transcript.MessageRow` is row-granular — one
`assistantToolCalls` row models the call set of one assistant turn, and a
mixed-content Rust message corresponds to adjacent rows. Intra-message
content ordering (`normalize_assistant_content_order`) is therefore NOT
modeled here; it is deferred to the `MessageKind` content-order extension
(follow-up to #448).

KNOWN GAP at that granularity: the row projection of a MIXED user message
(text + tool results in ONE message) is stricter than the Rust sanitizer.
Rust (`history.rs::drop_orphaned_tool_results`) filters a user message's
results against the pending block BEFORE clearing it for plain content, so
`User[Text, ToolResult(open)]` keeps the result; its row projection
`[ordinary, toolResult]` drops it (the ordinary row closes the block). The
owned loop never emits mixed user messages — results are threaded one per
message — so the gap is unreachable on the loop path, and the conformance
vectors are restricted to canonical single-content messages until the
content-order extension closes it.

Unlike `Compaction.PromptView.PairsClosedInMessages` (existence of a caller
*anywhere* in the list), the provider predicate here is ACTIVE-BLOCK aware:
an assistant tool-call row opens a pending result block, only matching
tool-results may appear while that block is open, and ordinary conversation
may resume only after every pending call is closed.
-/

namespace PromptAssembly

open Transcript (MessageRow MessageKind MessageRole)

/-- All call ids announced by assistant tool-call rows of `msgs`. -/
def callsIn (msgs : List MessageRow) : Finset ToolExecution.ToolCallId :=
  msgs.foldr
    (fun row acc =>
      match row.kind with
      | .assistantToolCalls callIds => callIds ∪ acc
      | _ => acc)
    ∅

/-- All call ids some tool-result row of `msgs` resolves. -/
def resolvedIn (msgs : List MessageRow) : Finset ToolExecution.ToolCallId :=
  msgs.foldr
    (fun row acc =>
      match row.kind with
      | .toolResult callId _ => insert callId acc
      | _ => acc)
    ∅

@[simp] theorem callsIn_nil : callsIn [] = ∅ := rfl

@[simp] theorem resolvedIn_nil : resolvedIn [] = ∅ := rfl

theorem callsIn_cons (row : MessageRow) (rest : List MessageRow) :
    callsIn (row :: rest) =
      (match row.kind with
        | .assistantToolCalls callIds => callIds ∪ callsIn rest
        | _ => callsIn rest) := rfl

theorem resolvedIn_cons (row : MessageRow) (rest : List MessageRow) :
    resolvedIn (row :: rest) =
      (match row.kind with
        | .toolResult callId _ => insert callId (resolvedIn rest)
        | _ => resolvedIn rest) := rfl

@[simp] theorem callsIn_cons_assistant (row : MessageRow) (rest : List MessageRow)
    (callIds : Finset ToolExecution.ToolCallId)
    (h : row.kind = .assistantToolCalls callIds) :
    callsIn (row :: rest) = callIds ∪ callsIn rest := by
  rw [callsIn_cons, h]

@[simp] theorem callsIn_cons_result (row : MessageRow) (rest : List MessageRow)
    (callId : ToolExecution.ToolCallId) (key : Transcript.ToolResultKey)
    (h : row.kind = .toolResult callId key) :
    callsIn (row :: rest) = callsIn rest := by
  rw [callsIn_cons, h]

@[simp] theorem callsIn_cons_ordinary (row : MessageRow) (rest : List MessageRow)
    (h : row.kind = .ordinary) :
    callsIn (row :: rest) = callsIn rest := by
  rw [callsIn_cons, h]

@[simp] theorem resolvedIn_cons_result (row : MessageRow) (rest : List MessageRow)
    (callId : ToolExecution.ToolCallId) (key : Transcript.ToolResultKey)
    (h : row.kind = .toolResult callId key) :
    resolvedIn (row :: rest) = insert callId (resolvedIn rest) := by
  rw [resolvedIn_cons, h]

@[simp] theorem resolvedIn_cons_assistant (row : MessageRow) (rest : List MessageRow)
    (callIds : Finset ToolExecution.ToolCallId)
    (h : row.kind = .assistantToolCalls callIds) :
    resolvedIn (row :: rest) = resolvedIn rest := by
  rw [resolvedIn_cons, h]

@[simp] theorem resolvedIn_cons_ordinary (row : MessageRow) (rest : List MessageRow)
    (h : row.kind = .ordinary) :
    resolvedIn (row :: rest) = resolvedIn rest := by
  rw [resolvedIn_cons, h]

theorem callsIn_append (a b : List MessageRow) :
    callsIn (a ++ b) = callsIn a ∪ callsIn b := by
  induction a with
  | nil => simp
  | cons row rest ih =>
    cases h : row.kind with
    | assistantToolCalls callIds =>
      rw [List.cons_append, callsIn_cons_assistant row _ callIds h,
        callsIn_cons_assistant row rest callIds h, ih, Finset.union_assoc]
    | toolResult callId key =>
      rw [List.cons_append, callsIn_cons_result row _ callId key h,
        callsIn_cons_result row rest callId key h, ih]
    | ordinary =>
      rw [List.cons_append, callsIn_cons_ordinary row _ h,
        callsIn_cons_ordinary row rest h, ih]

/-- Provider-validity from an active pending result block. `pending` is the
set of call ids announced by the current assistant tool-call row whose
tool-results must appear before ordinary conversation or another assistant
turn. -/
def ActiveBlockValidFrom (pending : Finset ToolExecution.ToolCallId) :
    List MessageRow → Prop
  | [] => pending = ∅
  | row :: rest =>
    match row.kind with
    | .toolResult callId _ =>
        callId ∈ pending ∧ ActiveBlockValidFrom (pending.erase callId) rest
    | .assistantToolCalls callIds =>
        pending = ∅ ∧ ActiveBlockValidFrom callIds rest
    | .ordinary =>
        pending = ∅ ∧ ActiveBlockValidFrom ∅ rest

abbrev ActiveBlockValid (msgs : List MessageRow) : Prop :=
  ActiveBlockValidFrom ∅ msgs

/-- Provider-valid history at row granularity: active-block discipline.
Strictly stronger than "ordered pairing in both directions" — it also
rejects ordinary rows interleaved mid-block, duplicate results for one
call, and late results for an already-closed block. (Content-order
canonicalization is deferred to the `MessageKind` content extension — see
the module docstring.) -/
structure ProviderValid (msgs : List MessageRow) : Prop where
  activeBlockValid : ActiveBlockValid msgs

/-- Each call id is announced by at most one assistant row: every announced
set is disjoint from all LATER announcements. (Two announcements of `c` at
positions `i < j` violate the disjointness recorded at `i`.) The system
invariant — call ids are uuids — under which soundness (T1) holds: without
it a later announcement could re-open an id from an abandoned block, so a
stale result would survive orphan-dropping and break the active-block
shape (e.g. `[call A, call A, result A]`). -/
def UniqueCallIds : List MessageRow → Prop
  | [] => True
  | row :: rest =>
    match row.kind with
    | .assistantToolCalls callIds =>
        Disjoint callIds (callsIn rest) ∧ UniqueCallIds rest
    | _ => UniqueCallIds rest

/-! Simp-reduction lemmas for the recursive predicates. -/
section Reduction

variable (row : MessageRow) (rest : List MessageRow)
variable (seen callIds : Finset ToolExecution.ToolCallId)
variable (callId : ToolExecution.ToolCallId) (key : Transcript.ToolResultKey)

@[simp] theorem activeBlockValidFrom_nil :
    ActiveBlockValidFrom seen [] ↔ seen = ∅ := by
  simp only [ActiveBlockValidFrom]

@[simp] theorem uniqueCallIds_nil : UniqueCallIds [] := trivial

theorem activeBlockValidFrom_cons_result (h : row.kind = .toolResult callId key) :
    ActiveBlockValidFrom seen (row :: rest) ↔
      callId ∈ seen ∧ ActiveBlockValidFrom (seen.erase callId) rest := by
  simp only [ActiveBlockValidFrom, h]

theorem activeBlockValidFrom_cons_assistant (h : row.kind = .assistantToolCalls callIds) :
    ActiveBlockValidFrom seen (row :: rest) ↔
      seen = ∅ ∧ ActiveBlockValidFrom callIds rest := by
  simp only [ActiveBlockValidFrom, h]

theorem activeBlockValidFrom_cons_ordinary (h : row.kind = .ordinary) :
    ActiveBlockValidFrom seen (row :: rest) ↔
      seen = ∅ ∧ ActiveBlockValidFrom ∅ rest := by
  simp only [ActiveBlockValidFrom, h]

theorem uniqueCallIds_cons_assistant (h : row.kind = .assistantToolCalls callIds) :
    UniqueCallIds (row :: rest) ↔
      Disjoint callIds (callsIn rest) ∧ UniqueCallIds rest := by
  simp only [UniqueCallIds, h]

theorem uniqueCallIds_cons_result (h : row.kind = .toolResult callId key) :
    UniqueCallIds (row :: rest) ↔ UniqueCallIds rest := by
  simp only [UniqueCallIds, h]

theorem uniqueCallIds_cons_ordinary (h : row.kind = .ordinary) :
    UniqueCallIds (row :: rest) ↔ UniqueCallIds rest := by
  simp only [UniqueCallIds, h]

end Reduction

/-- Uniqueness restricts to a suffix: the announcements of `b` are among the
announcements of `a ++ b`, so at-most-once is inherited. This is what makes
split-stability (T4) an instance of soundness — any pair-blind compaction
split's recent window inherits uniqueness from the whole transcript. -/
theorem UniqueCallIds.of_append_right :
    ∀ {a b : List MessageRow}, UniqueCallIds (a ++ b) → UniqueCallIds b := by
  intro a
  induction a with
  | nil => intro b h; simpa using h
  | cons row rest ih =>
    intro b h
    cases hk : row.kind with
    | assistantToolCalls callIds =>
      exact ih ((uniqueCallIds_cons_assistant row (rest ++ b) callIds hk).mp h).2
    | toolResult callId key =>
      exact ih ((uniqueCallIds_cons_result row (rest ++ b) callId key hk).mp h)
    | ordinary =>
      exact ih ((uniqueCallIds_cons_ordinary row (rest ++ b) hk).mp h)

end PromptAssembly
