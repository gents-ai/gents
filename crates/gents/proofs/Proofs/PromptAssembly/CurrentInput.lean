namespace PromptAssembly.CurrentInput

/-- The ownership facts needed at the provider boundary. Transcript content is
    intentionally absent: deduplication is structural, never text-based. -/
structure HistoryRow where
  requestId : Option String
  keyedSteeringInput : Bool
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
    (row.keyedSteeringInput || row.canonicalCurrentInput)

def providerHistory (currentRequestId : String) (rows : List HistoryRow) : List HistoryRow :=
  rows.filter fun row => !(belongsToCurrentInput currentRequestId row)

/-- The current steering input is supplied by `AgentRequest.content`, so its
    keyed transcript copy cannot also enter provider history. -/
theorem current_keyed_input_removed (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) true true ∉
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) true true] := by
  simp [providerHistory, belongsToCurrentInput]

/-- The canonical current input is excluded even for pre-key steering rows and
    ordinary request redrives. -/
theorem current_canonical_input_removed (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) false true ∉
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) false true] := by
  simp [providerHistory, belongsToCurrentInput]

/-- Other requests' steering inputs remain part of durable history. -/
theorem other_request_input_preserved (currentRequestId otherRequestId : String)
    (h : otherRequestId ≠ currentRequestId) :
    HistoryRow.mk (some otherRequestId) true true ∈
      providerHistory currentRequestId
        [HistoryRow.mk (some otherRequestId) true true] := by
  simp [providerHistory, belongsToCurrentInput, h]

/-- A tool result belongs to the current request but is not its canonical input,
    so redrive must preserve it. -/
theorem current_tool_result_preserved (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) false false ∈
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) false false] := by
  simp [providerHistory, belongsToCurrentInput]

/-- A background-completion notification may share request ownership, but is
    not mistaken for steering input. -/
theorem current_background_notification_preserved (currentRequestId : String) :
    HistoryRow.mk (some currentRequestId) false false ∈
      providerHistory currentRequestId
        [HistoryRow.mk (some currentRequestId) false false] := by
  simp [providerHistory, belongsToCurrentInput]

/-- Reapplying provider entry sanitation cannot delete any additional rows. -/
theorem provider_history_idempotent (currentRequestId : String)
    (rows : List HistoryRow) :
    providerHistory currentRequestId (providerHistory currentRequestId rows) =
      providerHistory currentRequestId rows := by
  simp [providerHistory]

end PromptAssembly.CurrentInput
