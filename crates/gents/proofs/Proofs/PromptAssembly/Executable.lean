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

/-! ## Per-turn resolution

`filterCallsBy` credits an announcement from the *global* resolved set. That is
what let a later turn reusing a call id resurrect an earlier unpaired
announcement (`Compaction.reused_call_id_breaks_prefix_stability`), and
production no longer does it: `drop_unpaired_tool_calls` scopes resolution to
the active turn via `resolved_keys_per_turn`, mirroring the `pending_calls`
reset in `drop_orphaned_tool_results`.

The two agree exactly under `UniqueCallIds` (`filterCallsByTurn_eq_filterCallsBy`
below), which is the hypothesis every theorem over `sanitize` already carries —
so the per-turn model is what production implements, and the global model
remains a sound over-approximation of it. -/

/-- Results closing the turn that just opened: the leading run of result rows.
A non-result row ends the turn, matching the `pending_calls` reset. -/
def resolvedInTurn : List MessageRow → Finset ToolExecution.ToolCallId
  | [] => ∅
  | row :: rest =>
    match row.kind with
    | .toolResult callId _ => insert callId (resolvedInTurn rest)
    | _ => ∅

/-- Credit an announcement only from its own turn's results. -/
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

/-- The provider sanitizer production implements. -/
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

/-- A turn's own results are among the list's results. -/
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
