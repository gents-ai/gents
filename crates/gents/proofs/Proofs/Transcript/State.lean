import Proofs.Basic
import Proofs.CanonicalOutput.State
import Proofs.ToolExecution.State
import Mathlib.Data.Finset.Basic

namespace Transcript

abbrev Sequence := Nat
abbrev MessageId := Nat
abbrev LogicalResultId := Nat
abbrev PayloadHash := Nat

structure ToolResultKey where
  sessionId : SessionId
  logicalResultId : LogicalResultId
  payloadHash : PayloadHash
  deriving DecidableEq, Repr

inductive MessageRole where
  | user
  | assistant
  deriving DecidableEq, Repr

namespace MessageRole

def toDefraDB : MessageRole → String
  | .user => "user"
  | .assistant => "assistant"

def fromDefraDB? : String → Option MessageRole
  | "user" => some .user
  | "assistant" => some .assistant
  | _ => none

theorem fromDefraDB_toDefraDB (role : MessageRole) :
    fromDefraDB? role.toDefraDB = some role := by
  cases role <;> rfl

end MessageRole

inductive MessageKind where
  | ordinary
  | assistantToolCalls (callIds : Finset ToolExecution.ToolCallId)
  | toolResult (callId : ToolExecution.ToolCallId) (key : ToolResultKey)
  deriving DecidableEq

namespace MessageKind

def referencesToolCall (kind : MessageKind) (callId : ToolExecution.ToolCallId) : Prop :=
  match kind with
  | .assistantToolCalls callIds => callId ∈ callIds
  | .toolResult resultCallId _ => resultCallId = callId
  | .ordinary => False

instance (kind : MessageKind) (callId : ToolExecution.ToolCallId) :
    Decidable (kind.referencesToolCall callId) := by
  unfold referencesToolCall
  cases kind <;> infer_instance

def toolResultKey? : MessageKind → Option ToolResultKey
  | .toolResult _ key => some key
  | _ => none

end MessageKind

/-- Sequencing/pairing projection of an immutable native message header.
`messageId` is assumed to be a fresh, genesis-validated Defra document identity
when a publication transition enters this model. This surface does not derive
identity from content or decide identity collisions. -/
structure MessageRow where
  messageId : MessageId
  sessionId : SessionId
  sequence : Sequence
  role : MessageRole
  kind : MessageKind
  deriving DecidableEq

namespace MessageRow

def isToolResultFor (row : MessageRow) (key : ToolResultKey) : Bool :=
  row.kind.toolResultKey? = some key

def reservesToolCall (row : MessageRow) (call : ToolExecution.ToolCallId)
    (sessionId : SessionId) (sequence : Sequence) : Prop :=
  row.sessionId = sessionId ∧
    row.sequence = sequence ∧
    row.role = .assistant ∧
    row.kind.referencesToolCall call

instance (row : MessageRow) (call : ToolExecution.ToolCallId)
    (sessionId : SessionId) (sequence : Sequence) :
    Decidable (row.reservesToolCall call sessionId sequence) := by
  unfold reservesToolCall
  infer_instance

end MessageRow

structure ToolCallRow where
  sessionId : SessionId
  callId : ToolExecution.ToolCallId
  messageSequence : Sequence
  state : ToolExecution.ToolCallState
  resultKey : Option ToolResultKey
  deriving DecidableEq, Repr

namespace ToolCallRow

def isPending (row : ToolCallRow) : Prop :=
  row.state = .pending

instance (row : ToolCallRow) : Decidable row.isPending := by
  unfold isPending
  infer_instance

def isRunning (row : ToolCallRow) : Prop :=
  row.state = .running

instance (row : ToolCallRow) : Decidable row.isRunning := by
  unfold isRunning
  infer_instance

def isCompleted (row : ToolCallRow) : Prop :=
  row.state = .completed

instance (row : ToolCallRow) : Decidable row.isCompleted := by
  unfold isCompleted
  infer_instance

end ToolCallRow

structure AssistantTurn where
  sessionId : SessionId
  sequence : Sequence
  /-- Provider order, retained by the pending-row suffix and later dispatch. -/
  callIds : List ToolExecution.ToolCallId
  /-- The canonical header owns this field. Transcript only uses it to select
  dispatchable versus terminal tool rows; bytes and closure validity stay in
  `CanonicalOutput`. -/
  outcome : CanonicalOutput.Outcome := .complete
  deriving DecidableEq

namespace AssistantTurn

def reservesToolCall (turn : AssistantTurn) (call : ToolCallRow) : Prop :=
  turn.sessionId = call.sessionId ∧
    turn.sequence = call.messageSequence ∧
    call.callId ∈ turn.callIds

instance (turn : AssistantTurn) (call : ToolCallRow) :
    Decidable (turn.reservesToolCall call) := by
  unfold reservesToolCall
  infer_instance

end AssistantTurn

structure TranscriptState where
  sessionId : SessionId
  nextSeq : Sequence
  messages : List MessageRow
  toolCalls : List ToolCallRow
  inFlight : Finset ToolExecution.ToolCallId
  deriving DecidableEq

def StrictlyIncreasingMessages : List MessageRow → Prop
  | [] => True
  | row :: rest =>
      (∀ other, other ∈ rest → row.sequence < other.sequence) ∧
        StrictlyIncreasingMessages rest

namespace TranscriptState

def messageCount (s : TranscriptState) : Nat :=
  s.messages.length

def toolCallCount (s : TranscriptState) : Nat :=
  s.toolCalls.length

def hasToolResultKey (s : TranscriptState) (key : ToolResultKey) : Bool :=
  s.messages.any (fun row => row.isToolResultFor key)

def toolResultMessageCount (s : TranscriptState) (key : ToolResultKey) : Nat :=
  (s.messages.filter (fun row => row.isToolResultFor key)).length

def toolCallById? (s : TranscriptState) (callId : ToolExecution.ToolCallId) :
    Option ToolCallRow :=
  s.toolCalls.find? (fun row => row.callId = callId)

def OrderedBySequence (s : TranscriptState) : Prop :=
  StrictlyIncreasingMessages s.messages

def ReservedByPersistedMessage (s : TranscriptState) (call : ToolCallRow) : Prop :=
  ∃ row, row ∈ s.messages ∧
    row.reservesToolCall call.callId call.sessionId call.messageSequence

def ToolCallReservedByMessage (s : TranscriptState) : Prop :=
  ∀ call, call ∈ s.toolCalls →
    ReservedByPersistedMessage s call

def DeliveredToolCallsPaired (s : TranscriptState) : Prop :=
  ∀ call, call ∈ s.toolCalls →
    ∀ key, call.resultKey = some key →
      s.toolResultMessageCount key = 1

def ToolResultMessagesPaired (s : TranscriptState) : Prop :=
  ∀ row, row ∈ s.messages →
    ∀ callId key, row.kind = .toolResult callId key →
      ∃ call, call ∈ s.toolCalls ∧
        call.callId = callId ∧
        call.resultKey = some key

def PairClosed (s : TranscriptState) : Prop :=
  s.ToolCallReservedByMessage ∧
    s.DeliveredToolCallsPaired ∧
    s.ToolResultMessagesPaired

def StrongDrain (s : TranscriptState) : Prop :=
  ∀ call, call ∈ s.toolCalls → call.state ≠ .running

def PublishableTurn (s : TranscriptState) (turn : AssistantTurn) : Prop :=
  turn.sessionId = s.sessionId ∧
    turn.sequence = s.nextSeq ∧
    turn.callIds.Nodup ∧
    ∀ call ∈ s.toolCalls, call.callId ∉ turn.callIds

instance (s : TranscriptState) (turn : AssistantTurn) :
    Decidable (s.PublishableTurn turn) := by
  unfold PublishableTurn
  infer_instance

/-- A call can enter host execution only from a pending row already reserved by
an immutable assistant header. -/
def Dispatchable (s : TranscriptState) (callId : ToolExecution.ToolCallId) : Prop :=
  ∃ call, call ∈ s.toolCalls ∧
    call.callId = callId ∧
    call.state = .pending ∧
    s.ReservedByPersistedMessage call

instance (s : TranscriptState) (callId : ToolExecution.ToolCallId) :
    Decidable (s.Dispatchable callId) := by
  unfold Dispatchable ReservedByPersistedMessage
  infer_instance

/-- Filtering pending rows preserves their publication order. Dispatching the
head therefore permits parallel startup without reordering calls. -/
def pendingCallIds (s : TranscriptState) : List ToolExecution.ToolCallId :=
  s.toolCalls.filterMap fun call =>
    if call.state = .pending then some call.callId else none

def ReadyToDispatch (s : TranscriptState) (callId : ToolExecution.ToolCallId) : Prop :=
  s.Dispatchable callId ∧ s.pendingCallIds.head? = some callId

instance (s : TranscriptState) (callId : ToolExecution.ToolCallId) :
    Decidable (s.ReadyToDispatch callId) := by
  unfold ReadyToDispatch
  infer_instance

def RunningPublishedCall (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) : Prop :=
  ∃ call, call ∈ s.toolCalls ∧
    call.callId = callId ∧
    call.state = .running ∧
    callId ∈ s.inFlight ∧
    s.ReservedByPersistedMessage call

instance (s : TranscriptState) (callId : ToolExecution.ToolCallId) :
    Decidable (s.RunningPublishedCall callId) := by
  unfold RunningPublishedCall ReservedByPersistedMessage
  infer_instance

def CancellablePublishedCall (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) : Prop :=
  ∃ call, call ∈ s.toolCalls ∧
    call.callId = callId ∧
    (call.state = .pending ∨ call.state = .running) ∧
    s.ReservedByPersistedMessage call

instance (s : TranscriptState) (callId : ToolExecution.ToolCallId) :
    Decidable (s.CancellablePublishedCall callId) := by
  unfold CancellablePublishedCall ReservedByPersistedMessage
  infer_instance

def replaceToolCall
    (rows : List ToolCallRow)
    (callId : ToolExecution.ToolCallId)
    (f : ToolCallRow → ToolCallRow) : List ToolCallRow :=
  rows.map fun row => if row.callId = callId then f row else row

def appendUserMessage (s : TranscriptState) (messageId : MessageId)
    (kind : MessageKind := .ordinary) : TranscriptState :=
  { s with
    nextSeq := s.nextSeq + 1
    messages := s.messages ++
      [{ messageId := messageId
       , sessionId := s.sessionId
       , sequence := s.nextSeq
       , role := .user
       , kind := kind }]
  }

def assistantKind (turn : AssistantTurn) : MessageKind :=
  if turn.callIds.isEmpty then .ordinary else .assistantToolCalls turn.callIds.toFinset

def toolRows (turn : AssistantTurn)
    (state : ToolExecution.ToolCallState) : List ToolCallRow :=
  turn.callIds.map fun callId =>
    { sessionId := turn.sessionId
    , callId := callId
    , messageSequence := turn.sequence
    , state := state
    , resultKey := none }

/-- Acceptance is one durable transaction at this abstraction boundary: the
already-closed Complete header and every ordered tool intent appear together,
all pending and none in flight. -/
def publishAcceptedAssistant (s : TranscriptState) (messageId : MessageId)
    (turn : AssistantTurn) : TranscriptState :=
  { s with
    nextSeq := s.nextSeq + 1
    messages := s.messages ++
      [{ messageId := messageId
       , sessionId := turn.sessionId
       , sequence := turn.sequence
       , role := .assistant
       , kind := assistantKind turn }]
    toolCalls := s.toolCalls ++ toolRows turn .pending
  }

/-- A terminal Partial header may preserve call provenance, but its rows are
born terminal and can never satisfy `Dispatchable`. -/
def publishPartialAssistant (s : TranscriptState) (messageId : MessageId)
    (turn : AssistantTurn) : TranscriptState :=
  { s with
    nextSeq := s.nextSeq + 1
    messages := s.messages ++
      [{ messageId := messageId
       , sessionId := turn.sessionId
       , sequence := turn.sequence
       , role := .assistant
       , kind := assistantKind turn }]
    toolCalls := s.toolCalls ++ toolRows turn .failed
  }

def dispatchToolCall (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) : TranscriptState :=
  { s with
    toolCalls := replaceToolCall s.toolCalls callId
      (fun row => { row with state := .running })
    inFlight := insert callId s.inFlight
  }

def dispatchToolCallWithMode (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) (mode : ToolExecution.AwaitMode) : TranscriptState :=
  let dispatched := s.dispatchToolCall callId
  match mode with
  | .foreground => dispatched
  | .background => { dispatched with inFlight := dispatched.inFlight.erase callId }

def releaseParentInFlight (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) : TranscriptState :=
  { s with inFlight := s.inFlight.erase callId }

def claimParentInFlight (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) : TranscriptState :=
  { s with inFlight := insert callId s.inFlight }

def publishToolResult (s : TranscriptState)
    (callId : ToolExecution.ToolCallId)
    (messageId : MessageId)
    (key : ToolResultKey)
    (terminal : ToolExecution.ToolCallState) : TranscriptState :=
  if s.hasToolResultKey key then s else
  { s with
    nextSeq := s.nextSeq + 1
    messages := s.messages ++
      [{ messageId := messageId
       , sessionId := s.sessionId
       , sequence := s.nextSeq
       , role := .user
       , kind := .toolResult callId key }]
    toolCalls := replaceToolCall s.toolCalls callId
      (fun row => { row with state := terminal, resultKey := some key })
    inFlight := s.inFlight.erase callId
  }

def completeToolWithResult (s : TranscriptState)
    (callId : ToolExecution.ToolCallId)
    (messageId : MessageId)
    (key : ToolResultKey) : TranscriptState :=
  s.publishToolResult callId messageId key .completed

def terminalizeToolCall (s : TranscriptState)
    (callId : ToolExecution.ToolCallId)
    (terminal : ToolExecution.ToolCallState) : TranscriptState :=
  { s with
    toolCalls := replaceToolCall s.toolCalls callId
      (fun row => { row with state := terminal })
    inFlight := s.inFlight.erase callId
  }

/-- Request finalization reuses the tool owner's cancel-before-dispatch policy:
only exact accepted call identities that are still pending are cancelled. -/
def cancelPendingOwnedCalls (s : TranscriptState)
    (owned : List (SessionId × Sequence × ToolExecution.ToolCallId)) : TranscriptState :=
  { s with toolCalls := s.toolCalls.map fun row =>
      if (row.sessionId, row.messageSequence, row.callId) ∈ owned &&
          row.state == .pending then
        { row with state := .cancelled }
      else row }

def abandonHookOwnership (s : TranscriptState) : TranscriptState :=
  { s with inFlight := ∅ }

end TranscriptState

end Transcript
