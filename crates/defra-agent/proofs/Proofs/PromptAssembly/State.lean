import Proofs.Basic
import Proofs.Transcript.State

/-!
# PromptAssembly State (issue #448)

Vocabulary for the provider-input narrowing model: the durable transcript is
*permissive* (`Transcript.PairClosed` only constrains completed calls), while
providers reject any assistant tool-call not followed by its result and any
tool-result not preceded by its call. `sanitize` (Rust:
`compaction::sanitize_history_for_provider`, applied at the
`run_loop_stream` entry chokepoint) narrows the former to the latter.

Granularity note: a `Transcript.MessageRow` is row-granular — one
`assistantToolCalls` row models the call set of one assistant turn, and a
mixed-content Rust message corresponds to adjacent rows. Intra-message
content ordering (`normalize_assistant_content_order`) is therefore NOT
modeled here; it is deferred to the `MessageKind` content-order extension
(follow-up to #448).

Unlike `Compaction.PromptView.PairsClosedInMessages` (existence of a caller
*anywhere* in the list), the provider predicates here are ORDER-AWARE —
results must follow their calls and calls must be followed by their results —
and are defined recursively so the sanitizer theorems are structural
inductions.
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

/-- Every tool-result row's call id was announced by an EARLIER row, with
`seen` carrying the call ids announced before the current suffix. Stricter
than `Compaction.PromptView.PairsClosedInMessages` (existence anywhere). -/
def ResultsFollowCallsFrom (seen : Finset ToolExecution.ToolCallId) :
    List MessageRow → Prop
  | [] => True
  | row :: rest =>
    match row.kind with
    | .toolResult callId _ =>
        callId ∈ seen ∧ ResultsFollowCallsFrom seen rest
    | .assistantToolCalls callIds =>
        ResultsFollowCallsFrom (seen ∪ callIds) rest
    | .ordinary => ResultsFollowCallsFrom seen rest

abbrev ResultsFollowCalls (msgs : List MessageRow) : Prop :=
  ResultsFollowCallsFrom ∅ msgs

/-- Every announced call id is resolved by a tool-result row AFTER the
announcing row — the provider-side requirement on assistant tool calls. -/
def CallsFollowedByResults : List MessageRow → Prop
  | [] => True
  | row :: rest =>
    match row.kind with
    | .assistantToolCalls callIds =>
        callIds ⊆ resolvedIn rest ∧ CallsFollowedByResults rest
    | _ => CallsFollowedByResults rest

/-- Provider-valid history at row granularity: ordered pairing in both
directions. (Content-order canonicalization is deferred to the
`MessageKind` content extension — see the module docstring.) -/
structure ProviderValid (msgs : List MessageRow) : Prop where
  resultsFollowCalls : ResultsFollowCalls msgs
  callsFollowedByResults : CallsFollowedByResults msgs

/-- Each call id is announced by at most one assistant row: every announced
set is disjoint from all LATER announcements. (Two announcements of `c` at
positions `i < j` violate the disjointness recorded at `i`.) The system
invariant — call ids are uuids — under which the `followed-by-results`
direction of soundness holds. -/
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

@[simp] theorem resultsFollowFrom_nil : ResultsFollowCallsFrom seen [] := trivial

@[simp] theorem callsFollowed_nil : CallsFollowedByResults [] := trivial

@[simp] theorem uniqueCallIds_nil : UniqueCallIds [] := trivial

theorem resultsFollowFrom_cons_result (h : row.kind = .toolResult callId key) :
    ResultsFollowCallsFrom seen (row :: rest) ↔
      callId ∈ seen ∧ ResultsFollowCallsFrom seen rest := by
  simp only [ResultsFollowCallsFrom, h]

theorem resultsFollowFrom_cons_assistant (h : row.kind = .assistantToolCalls callIds) :
    ResultsFollowCallsFrom seen (row :: rest) ↔
      ResultsFollowCallsFrom (seen ∪ callIds) rest := by
  simp only [ResultsFollowCallsFrom, h]

theorem resultsFollowFrom_cons_ordinary (h : row.kind = .ordinary) :
    ResultsFollowCallsFrom seen (row :: rest) ↔
      ResultsFollowCallsFrom seen rest := by
  simp only [ResultsFollowCallsFrom, h]

theorem callsFollowed_cons_assistant (h : row.kind = .assistantToolCalls callIds) :
    CallsFollowedByResults (row :: rest) ↔
      callIds ⊆ resolvedIn rest ∧ CallsFollowedByResults rest := by
  simp only [CallsFollowedByResults, h]

theorem callsFollowed_cons_result (h : row.kind = .toolResult callId key) :
    CallsFollowedByResults (row :: rest) ↔ CallsFollowedByResults rest := by
  simp only [CallsFollowedByResults, h]

theorem callsFollowed_cons_ordinary (h : row.kind = .ordinary) :
    CallsFollowedByResults (row :: rest) ↔ CallsFollowedByResults rest := by
  simp only [CallsFollowedByResults, h]

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
