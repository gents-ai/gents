namespace PromptAssembly.CurrentInput

variable {α : Type}

/-- A successor retry continues the durable provider projection only after the
parent's authored input is proven present. A failure before input publication
still needs its original prompt. Tool effects remain owned by their durable
call records; selecting input never executes a tool. -/
def admitResume (scopedTerminal settledTools published : Bool) : Option Bool :=
  if scopedTerminal && settledTools then some published else none

theorem unsettled_tools_cannot_resume (scopedTerminal published : Bool) :
    admitResume scopedTerminal false published = none := by
  simp [admitResume]

def entryMessages (resume : Bool) (history : List α) (authored : α) : List α :=
  if resume then history else history ++ [authored]

theorem retry_preserves_durable_frontier (history : List α) (authored : α) :
    entryMessages true history authored = history := rfl

theorem fresh_input_appended_once (history : List α) (authored : α) :
    entryMessages false history authored = history ++ [authored] := rfl

/-- Providers requiring a user tail receive a controller continuation after a
completed assistant frontier, never another copy of the original instruction. -/
def needsContinuation (resume assistantTail : Bool) : Bool := resume && assistantTail

theorem fresh_request_needs_no_continuation (assistantTail : Bool) :
    needsContinuation false assistantTail = false := rfl

def entryWithContext (resume : Bool) (history : List α) (authored continuation : α)
    (context : Option α) (assistantTail : Bool) : List α :=
  if !resume then entryMessages false history authored
  else match context with
    | some current => history ++ [current]
    | none => if needsContinuation resume assistantTail then history ++ [continuation] else history

theorem retry_ignores_original_prompt (history : List α) (first second continuation : α)
    (context : Option α) (assistantTail : Bool) :
    entryWithContext true history first continuation context assistantTail =
    entryWithContext true history second continuation context assistantTail := rfl

def publishesAuthoredInput (resume : Bool) : Bool := !resume

theorem retry_does_not_republish_input : publishesAuthoredInput true = false := rfl

/-- The ownership facts needed at the provider boundary. Transcript content is
    intentionally absent: deduplication is structural, never text-based. -/
structure HistoryRow where
  requestId : Option String
  canonicalCurrentInput : Bool
  deriving DecidableEq

/-- The prompt hook appends only when the request-scoped canonical message is
    absent. Atomic steering persists that same canonical encoding. -/
def hookAppendsPrompt (canonicalInputExists : Bool) : Bool :=
  !canonicalInputExists

theorem canonical_steering_input_prevents_duplicate_hook_row :
    hookAppendsPrompt true = false := by
  rfl

def belongsToCurrentInput (currentRequestId : String) (row : HistoryRow) : Bool :=
  row.requestId == some currentRequestId &&
    row.canonicalCurrentInput

def providerHistory (currentRequestId : String) (rows : List HistoryRow) : List HistoryRow :=
  rows.filter fun row => !(belongsToCurrentInput currentRequestId row)

/-- The canonical current input is excluded on request redrive. -/
theorem current_canonical_input_removed (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) true ∉
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) true] := by
  simp [providerHistory, belongsToCurrentInput]

/-- Other requests' steering inputs remain part of durable history. -/
theorem other_request_input_preserved (currentRequestId otherRequestId : String)
    (h : otherRequestId ≠ currentRequestId) :
    HistoryRow.mk (some otherRequestId) true ∈
      providerHistory currentRequestId
        [HistoryRow.mk (some otherRequestId) true] := by
  simp [providerHistory, belongsToCurrentInput, h]

/-- A tool result belongs to the current request but is not its canonical input,
    so redrive must preserve it. -/
theorem current_tool_result_preserved (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) false ∈
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) false] := by
  simp [providerHistory, belongsToCurrentInput]

/-- A background-completion notification may share request ownership, but is
    not mistaken for steering input. -/
theorem current_background_notification_preserved (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) false ∈
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) false] := by
  simp [providerHistory, belongsToCurrentInput]

/-- Reapplying provider entry sanitation cannot delete any additional rows. -/
theorem provider_history_idempotent (currentRequestId : String)
    (rows : List HistoryRow) :
    providerHistory currentRequestId (providerHistory currentRequestId rows) =
      providerHistory currentRequestId rows := by
  simp [providerHistory]

end PromptAssembly.CurrentInput
