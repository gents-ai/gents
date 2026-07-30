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

def sanitize (msgs : List MessageRow) : List MessageRow :=
  dropUnpairedCalls (dropOrphanedResults msgs)

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
  | preamble
  | summaryReminder
  | skillReminder (index : Nat)
  | conversation (index : Nat)
  | contextPreamble
  | prompt
  deriving DecidableEq, Repr

def buildLayers (summaryCount conversationLen : Nat) : List Slot :=
  (if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
    (List.range conversationLen).map Slot.conversation

def injectSkills (skillCount : Nat) (layers : List Slot) : List Slot :=
  (List.range skillCount).map Slot.skillReminder ++ layers

def perTurnRequest (layers : List Slot) : List Slot :=
  Slot.preamble :: layers ++ [Slot.prompt]

def assemble (skillCount summaryCount conversationLen : Nat) : List Slot :=
  perTurnRequest (injectSkills skillCount (buildLayers summaryCount conversationLen))

end PromptAssembly
