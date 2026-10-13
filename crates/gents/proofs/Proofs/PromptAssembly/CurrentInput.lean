namespace PromptAssembly.CurrentInput

variable {α : Type}

/-- A successor retry continues the durable provider projection only after the
parent's authored input is proven present. A failure before input publication
still needs its original prompt. Tool effects remain owned by their durable
call records; selecting input never executes a tool. -/
def admitResume (scopedTerminal sameSession sameRequester settledTools published : Bool) : Option Bool :=
  if !scopedTerminal then none
  else if !sameSession then some false
  else if sameRequester && settledTools then some published else none

theorem unsettled_tools_cannot_resume (scopedTerminal sameRequester published : Bool) :
    admitResume scopedTerminal true sameRequester false published = none := by
  cases scopedTerminal <;> simp [admitResume]

/-- Stale-request resend creates a new session. Retry lineage is provenance,
not authority to import the old session's history. -/
theorem new_session_starts_fresh (sameRequester settledTools published : Bool) :
    admitResume true false sameRequester settledTools published = some false := rfl

/-- Background execution has its own receipt and delivery owners and may
outlive the request. Only unsettled foreground execution blocks continuation. -/
def settledForResume (running background : Bool) : Bool := !running || background

theorem background_does_not_block_resume (running : Bool) :
    settledForResume running true = true := by simp [settledForResume]

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

/-- Queued messages a turn answers (`SessionQueue.foldRun`). Folded messages
are consumed only by publishing them as authored input, so a retry resuming
its parent's durable progress answers none of the run its claim selected:
those messages stay queued for the next turn. A parent's already published
folded messages are durable history the resumed retry continues from. -/
def answersSelection (resume : Bool) (selected : List α) : List α :=
  if publishesAuthoredInput resume then selected else []

theorem resumed_retry_answers_no_selection (selected : List α) :
    answersSelection true selected = [] := rfl

theorem fresh_turn_answers_its_selection (selected : List α) :
    answersSelection false selected = selected := rfl

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

/-- A completed wake acknowledges its claim receipt. Failed or interrupted
wakes do not acknowledge delivery. Claim receipts contain the complete key
owned prefix, so subtraction from the latest completed receipt selects this
wake's new input without changing notification publication or transcript order.
Fork imports have no request membership and stay in retained history. -/
def wakeNotificationKeys (claimed acknowledged : List String) : List String :=
  claimed.filter fun key => !acknowledged.contains key

theorem acknowledged_notification_is_not_current (claimed acknowledged : List String)
    (key : String) (h : key ∈ acknowledged) :
    key ∉ wakeNotificationKeys claimed acknowledged := by
  simp [wakeNotificationKeys, h]

/-- Stable partition at provider entry, after canonical history reduction.
Only physical headers selected by the claim receipt move; text equality and
provider IDs confer no ownership. The canonical transcript remains untouched. -/
def wakeHistory (current : α → Bool) (rows : List α) : List α :=
  rows.filter (fun row => !current row) ++ rows.filter current

/-- Current notification input is reconstructed from its canonical headers
even when a prior compaction removed those rows from retained history. -/
def wakeEntry (current : α → Bool) (history notifications : List α) : List α :=
  history.filter (fun row => !current row) ++ notifications

theorem retained_wake_entry_is_stable_partition (current : α → Bool) (rows : List α) :
    wakeEntry current rows (rows.filter current) = wakeHistory current rows := rfl

theorem wake_history_preserves_length (current : α → Bool) (rows : List α) :
    (wakeHistory current rows).length = rows.length := by
  induction rows with
  | nil => simp [wakeHistory]
  | cons row rows ih =>
    cases h : current row <;> simp_all [wakeHistory, List.filter_cons, ← Nat.add_assoc]

theorem wake_history_preserves_membership (current : α → Bool) (rows : List α) (row : α) :
    row ∈ wakeHistory current rows ↔ row ∈ rows := by
  cases h : current row <;> simp [wakeHistory, h]

theorem wake_history_preserves_prior_order (current : α → Bool) (rows : List α) :
    (wakeHistory current rows).filter (fun row => !current row) =
      rows.filter (fun row => !current row) := by
  simp [wakeHistory, List.filter_filter]

end PromptAssembly.CurrentInput
