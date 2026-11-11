import Proofs.PromptAssembly.State

namespace PromptAssembly

open Transcript (MessageRow MessageKind MessageRole)

def withKind (row : MessageRow) (kind : MessageKind) : MessageRow :=
  { row with kind := kind }

@[simp] theorem withKind_kind (row : MessageRow) (kind : MessageKind) :
    (withKind row kind).kind = kind := rfl

theorem withKind_self (row : MessageRow) (kind : MessageKind)
    (h : row.kind = kind) : withKind row kind = row := by
  cases row
  cases h
  rfl

def dropOrphanedFrom (pending : Finset ToolExecution.ToolCallId) :
    List MessageRow → List MessageRow
  | [] => []
  | row :: rest =>
    match row.kind with
    | .toolResult callId _ =>
        if callId ∈ pending then row :: dropOrphanedFrom (pending.erase callId) rest
        else dropOrphanedFrom pending rest
    | .assistantToolCalls callIds =>
        row :: dropOrphanedFrom callIds rest
    | .ordinary => row :: dropOrphanedFrom ∅ rest

def dropOrphanedResults (msgs : List MessageRow) : List MessageRow :=
  dropOrphanedFrom ∅ msgs

def filterCallsBy (resolved : Finset ToolExecution.ToolCallId) :
    List MessageRow → List MessageRow
  | [] => []
  | row :: rest =>
    match row.kind with
    | .assistantToolCalls callIds =>
        if callIds ∩ resolved = ∅ then filterCallsBy resolved rest
        else
          withKind row (.assistantToolCalls (callIds ∩ resolved)) ::
            filterCallsBy resolved rest
    | .toolResult _ _ => row :: filterCallsBy resolved rest
    | .ordinary => row :: filterCallsBy resolved rest

def dropUnpairedCalls (msgs : List MessageRow) : List MessageRow :=
  filterCallsBy (resolvedIn msgs) msgs

/-- Global-resolution proof model, valid as a runtime abstraction only under
unique call IDs. Implement provider narrowing with `sanitizeTurn`. -/
def sanitizeGlobal (msgs : List MessageRow) : List MessageRow :=
  dropUnpairedCalls (dropOrphanedResults msgs)

/-! ## Per-turn resolution -/

/-- Results closing the turn that just opened: the leading run of result rows. -/
def resolvedInTurn : List MessageRow → Finset ToolExecution.ToolCallId
  | [] => ∅
  | row :: rest =>
    match row.kind with
    | .toolResult callId _ => insert callId (resolvedInTurn rest)
    | _ => ∅

def filterCallsByTurn : List MessageRow → List MessageRow
  | [] => []
  | row :: rest =>
    match row.kind with
    | .assistantToolCalls callIds =>
        if callIds ∩ resolvedInTurn rest = ∅ then filterCallsByTurn rest
        else withKind row (.assistantToolCalls (callIds ∩ resolvedInTurn rest))
          :: filterCallsByTurn rest
    | _ => row :: filterCallsByTurn rest

def dropUnpairedCallsTurn (msgs : List MessageRow) : List MessageRow :=
  filterCallsByTurn msgs

def sanitizeTurn (msgs : List MessageRow) : List MessageRow :=
  dropUnpairedCallsTurn (dropOrphanedResults msgs)

section TurnReduction

variable (row : MessageRow) (rest : List MessageRow)
variable (callIds : Finset ToolExecution.ToolCallId)
variable (callId : ToolExecution.ToolCallId) (key : Transcript.ToolResultKey)

@[simp] theorem resolvedInTurn_nil : resolvedInTurn [] = ∅ := rfl

theorem resolvedInTurn_cons_result (h : row.kind = .toolResult callId key) :
    resolvedInTurn (row :: rest) = insert callId (resolvedInTurn rest) := by
  simp only [resolvedInTurn, h]

theorem resolvedInTurn_cons_assistant (h : row.kind = .assistantToolCalls callIds) :
    resolvedInTurn (row :: rest) = ∅ := by
  simp only [resolvedInTurn, h]

theorem resolvedInTurn_cons_ordinary (h : row.kind = .ordinary) :
    resolvedInTurn (row :: rest) = ∅ := by
  simp only [resolvedInTurn, h]

@[simp] theorem filterCallsByTurn_nil : filterCallsByTurn [] = [] := rfl

theorem filterCallsByTurn_cons_assistant (h : row.kind = .assistantToolCalls callIds) :
    filterCallsByTurn (row :: rest) =
      if callIds ∩ resolvedInTurn rest = ∅ then filterCallsByTurn rest
      else withKind row (.assistantToolCalls (callIds ∩ resolvedInTurn rest))
        :: filterCallsByTurn rest := by
  simp only [filterCallsByTurn, h]

theorem filterCallsByTurn_cons_result (h : row.kind = .toolResult callId key) :
    filterCallsByTurn (row :: rest) = row :: filterCallsByTurn rest := by
  simp only [filterCallsByTurn, h]

theorem filterCallsByTurn_cons_ordinary (h : row.kind = .ordinary) :
    filterCallsByTurn (row :: rest) = row :: filterCallsByTurn rest := by
  simp only [filterCallsByTurn, h]

end TurnReduction

theorem resolvedInTurn_subset_resolvedIn (l : List MessageRow) :
    resolvedInTurn l ⊆ resolvedIn l := by
  induction l with
  | nil => simp
  | cons row rest ih =>
    cases hk : row.kind with
    | toolResult callId key =>
      rw [resolvedInTurn_cons_result row rest callId key hk,
        resolvedIn_cons_result row rest callId key hk]
      exact Finset.insert_subset_insert _ ih
    | assistantToolCalls callIds =>
      rw [resolvedInTurn_cons_assistant row rest callIds hk]
      exact Finset.empty_subset _
    | ordinary =>
      rw [resolvedInTurn_cons_ordinary row rest hk]
      exact Finset.empty_subset _

section Reduction

variable (row : MessageRow) (rest : List MessageRow)
variable (pending resolved callIds : Finset ToolExecution.ToolCallId)
variable (callId : ToolExecution.ToolCallId) (key : Transcript.ToolResultKey)

@[simp] theorem dropOrphanedFrom_nil : dropOrphanedFrom pending [] = [] := rfl

@[simp] theorem filterCallsBy_nil : filterCallsBy resolved [] = [] := rfl

theorem dropOrphanedFrom_cons_result (h : row.kind = .toolResult callId key) :
    dropOrphanedFrom pending (row :: rest) =
      if callId ∈ pending then row :: dropOrphanedFrom (pending.erase callId) rest
      else dropOrphanedFrom pending rest := by
  simp only [dropOrphanedFrom, h]

theorem dropOrphanedFrom_cons_assistant (h : row.kind = .assistantToolCalls callIds) :
    dropOrphanedFrom pending (row :: rest) =
      row :: dropOrphanedFrom callIds rest := by
  simp only [dropOrphanedFrom, h]

theorem dropOrphanedFrom_cons_ordinary (h : row.kind = .ordinary) :
    dropOrphanedFrom pending (row :: rest) = row :: dropOrphanedFrom ∅ rest := by
  simp only [dropOrphanedFrom, h]

theorem filterCallsBy_cons_assistant (h : row.kind = .assistantToolCalls callIds) :
    filterCallsBy resolved (row :: rest) =
      if callIds ∩ resolved = ∅ then filterCallsBy resolved rest
      else
        withKind row (.assistantToolCalls (callIds ∩ resolved)) ::
          filterCallsBy resolved rest := by
  simp only [filterCallsBy, h]

theorem filterCallsBy_cons_result (h : row.kind = .toolResult callId key) :
    filterCallsBy resolved (row :: rest) = row :: filterCallsBy resolved rest := by
  simp only [filterCallsBy, h]

theorem filterCallsBy_cons_ordinary (h : row.kind = .ordinary) :
    filterCallsBy resolved (row :: rest) = row :: filterCallsBy resolved rest := by
  simp only [filterCallsBy, h]

end Reduction

inductive Slot where
  | preamble (instructions : Option String)
  | summaryReminder
  | skillReminder (index : Nat)
  | conversation (index : Nat)
  | prompt
  deriving DecidableEq, Repr

def buildLayers (summaryCount conversationLen : Nat) : List Slot :=
  (if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
    (List.range conversationLen).map Slot.conversation

def injectSkills (skillCount : Nat) (layers : List Slot) : List Slot :=
  (List.range skillCount).map Slot.skillReminder ++ layers

def perTurnRequest (layers : List Slot) (instructions : Option String := none) : List Slot :=
  Slot.preamble instructions :: layers ++ [Slot.prompt]

def assemble (skillCount summaryCount conversationLen : Nat)
    (instructions : Option String := none) : List Slot :=
  perTurnRequest (injectSkills skillCount (buildLayers summaryCount conversationLen)) instructions

end PromptAssembly
