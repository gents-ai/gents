import Proofs.Basic
import Proofs.Transcript.State

namespace PromptAssembly

open Transcript (MessageRow MessageKind MessageRole)

def callsIn (msgs : List MessageRow) : Finset ToolExecution.ToolCallId :=
  msgs.foldr
    (fun row acc =>
      match row.kind with
      | .assistantToolCalls callIds => callIds ∪ acc
      | _ => acc)
    ∅

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

structure ProviderValid (msgs : List MessageRow) : Prop where
  activeBlockValid : ActiveBlockValid msgs

def UniqueCallIds : List MessageRow → Prop
  | [] => True
  | row :: rest =>
    match row.kind with
    | .assistantToolCalls callIds =>
        Disjoint callIds (callsIn rest) ∧ UniqueCallIds rest
    | _ => UniqueCallIds rest

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
