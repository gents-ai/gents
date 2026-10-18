namespace PromptAssembly.CurrentInput

variable {α : Type}





def admitResume (scopedTerminal sameSession sameRequester settledTools published : Bool) : Option Bool :=
  if !scopedTerminal then none
  else if !sameSession then some false
  else if sameRequester && settledTools then some published else none

theorem unsettled_tools_cannot_resume (scopedTerminal sameRequester published : Bool) :
    admitResume scopedTerminal true sameRequester false published = none := by
  cases scopedTerminal <;> simp [admitResume]



theorem new_session_starts_fresh (sameRequester settledTools published : Bool) :
    admitResume true false sameRequester settledTools published = some false := rfl



def settledForResume (running background : Bool) : Bool := !running || background

theorem background_does_not_block_resume (running : Bool) :
    settledForResume running true = true := by simp [settledForResume]

def entryMessages (resume : Bool) (history : List α) (authored : α) : List α :=
  if resume then history else history ++ [authored]

theorem retry_preserves_durable_frontier (history : List α) (authored : α) :
    entryMessages true history authored = history := rfl

theorem fresh_input_appended_once (history : List α) (authored : α) :
    entryMessages false history authored = history ++ [authored] := rfl



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






def answersSelection (resume : Bool) (selected : List α) : List α :=
  if publishesAuthoredInput resume then selected else []

theorem resumed_retry_answers_no_selection (selected : List α) :
    answersSelection true selected = [] := rfl

theorem fresh_turn_answers_its_selection (selected : List α) :
    answersSelection false selected = selected := rfl



structure HistoryRow where
  requestId : Option String
  canonicalCurrentInput : Bool
  deriving DecidableEq



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


theorem current_canonical_input_removed (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) true ∉
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) true] := by
  simp [providerHistory, belongsToCurrentInput]


theorem other_request_input_preserved (currentRequestId otherRequestId : String)
    (h : otherRequestId ≠ currentRequestId) :
    HistoryRow.mk (some otherRequestId) true ∈
      providerHistory currentRequestId
        [HistoryRow.mk (some otherRequestId) true] := by
  simp [providerHistory, belongsToCurrentInput, h]



theorem current_tool_result_preserved (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) false ∈
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) false] := by
  simp [providerHistory, belongsToCurrentInput]



theorem current_background_notification_preserved (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) false ∈
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) false] := by
  simp [providerHistory, belongsToCurrentInput]


theorem provider_history_idempotent (currentRequestId : String)
    (rows : List HistoryRow) :
    providerHistory currentRequestId (providerHistory currentRequestId rows) =
      providerHistory currentRequestId rows := by
  simp [providerHistory]






def wakeNotificationKeys (claimed acknowledged : List String) : List String :=
  claimed.filter fun key => !acknowledged.contains key

theorem acknowledged_notification_is_not_current (claimed acknowledged : List String)
    (key : String) (h : key ∈ acknowledged) :
    key ∉ wakeNotificationKeys claimed acknowledged := by
  simp [wakeNotificationKeys, h]




def wakeHistory (current : α → Bool) (rows : List α) : List α :=
  rows.filter (fun row => !current row) ++ rows.filter current



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
