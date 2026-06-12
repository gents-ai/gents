import Proofs.PromptAssembly.State

/-!
# PromptAssembly Executable (issue #448)

Executable mirrors of the Rust provider-input pipeline, in the exact
composition order the code uses (`compaction.rs::sanitize_history_for_provider`):

1. `dropOrphanedResults` — drop tool-result rows that are not closing the
   currently active assistant tool-call block (Rust
   `history::drop_orphaned_tool_results`). ORDER MATTERS: this must run FIRST
   — see `Properties.lean`: the swapped composition keeps a call on the
   strength of a result it is about to drop, and an unpaired call reaches the
   provider (the counterexample that fixed the Rust order).
2. `dropUnpairedCalls` — intersect each assistant row's call set with the
   resolved set of the whole (orphan-free) list, dropping rows left empty
   (Rust `history::drop_unpaired_tool_calls`).

(The Rust pipeline composes a third stage,
`history::normalize_assistant_content_order` — intra-message content
ordering, below row granularity and deferred to the `MessageKind`
content-order extension; see the granularity note in `State.lean`.)

Plus the prompt-layer assembly mirrors (`LayeredPromptBuilder::build`, the
skill-reminder injection in `daemon/request.rs`, and the per-turn request
shape of `loop_stream::build_request`), so the layer order is fixed by
definition and any reordering is a Lean-breaking change.

Rust conformance: `crates/defra-agent/tests/conformance/prompt_assembly.rs`
runs the shared vectors through `sanitize_history_for_provider`.
-/

namespace PromptAssembly

open Transcript (MessageRow MessageKind MessageRole)

/-- Replace a row's kind, keeping its identity fields. -/
def withKind (row : MessageRow) (kind : MessageKind) : MessageRow :=
  { row with kind := kind }

@[simp] theorem withKind_kind (row : MessageRow) (kind : MessageKind) :
    (withKind row kind).kind = kind := rfl

theorem withKind_self (row : MessageRow) (kind : MessageKind)
    (h : row.kind = kind) : withKind row kind = row := by
  cases row
  cases h
  rfl

/-- Keep a tool-result row only if its call id is currently pending. Assistant
rows replace the pending block; ordinary rows close it. -/
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

/-- Narrow each assistant row's announced set to `resolved`, dropping rows
left empty (an assistant turn that was nothing but unpaired calls). Results
and ordinary rows pass through untouched. -/
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

/-- The provider-input narrowing map, in the Rust composition order:
orphans first, then unpaired calls. -/
def sanitize (msgs : List MessageRow) : List MessageRow :=
  dropUnpairedCalls (dropOrphanedResults msgs)

/-! Reduction lemmas. -/
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

/-! ## Prompt-layer assembly -/

/-- One slot of the assembled provider input, in the layer vocabulary of
`prompt.rs` (layers 1+2 = preamble, 3 = compaction-summary reminder,
skills = per-turn skill reminders, 4 = conversation, prompt last). -/
inductive Slot where
  | preamble
  | summaryReminder
  | skillReminder (index : Nat)
  | conversation (index : Nat)
  | prompt
  deriving DecidableEq, Repr

/-- `LayeredPromptBuilder::build`: the summary reminder (when any summaries
exist) leads the conversation. -/
def buildLayers (summaryCount conversationLen : Nat) : List Slot :=
  (if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
    (List.range conversationLen).map Slot.conversation

/-- `daemon/request.rs`: skill reminders are PREPENDED to the built layers. -/
def injectSkills (skillCount : Nat) (layers : List Slot) : List Slot :=
  (List.range skillCount).map Slot.skillReminder ++ layers

/-- `loop_stream::build_request`: the preamble leads as a system message and
the new prompt rides last. -/
def perTurnRequest (layers : List Slot) : List Slot :=
  Slot.preamble :: layers ++ [Slot.prompt]

/-- The full assembly for one request, mirroring the daemon pipeline. -/
def assemble (skillCount summaryCount conversationLen : Nat) : List Slot :=
  perTurnRequest (injectSkills skillCount (buildLayers summaryCount conversationLen))

end PromptAssembly
