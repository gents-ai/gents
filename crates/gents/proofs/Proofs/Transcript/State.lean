import Proofs.Basic
import Proofs.ToolExecution.State
import Mathlib.Data.Finset.Basic

/-!
# Transcript State

Durable transcript vocabulary for one session. This model abstracts over
`AgentMessage` and `AgentToolCall` rows, preserving only the fields needed for
ordering, tool-result dedupe, and pair closure.
-/

namespace Transcript

abbrev Sequence := Nat
abbrev MessageId := Nat
abbrev LogicalResultId := Nat
abbrev PayloadHash := Nat

/-- Abstract version of #160's stable tool-result message key inputs. -/
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

/-- Transcript-relevant shape of a persisted message. -/
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

def isCompleted (row : ToolCallRow) : Prop :=
  row.state = .completed

instance (row : ToolCallRow) : Decidable row.isCompleted := by
  unfold isCompleted
  infer_instance

end ToolCallRow

/-- The runtime can reserve an assistant message sequence before persisting the
assistant `AgentMessage`; this mirrors `TranscriptTurnState::AssistantBuilding`.
-/
structure AssistantTurn where
  sessionId : SessionId
  sequence : Sequence
  callIds : Finset ToolExecution.ToolCallId
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
  assistantTurn : Option AssistantTurn
  deriving DecidableEq

def StrictlyIncreasingMessages : List MessageRow → Prop
  | [] => True
  | row :: rest =>
      (∀ other, other ∈ rest → row.sequence < other.sequence) ∧
        StrictlyIncreasingMessages rest

def UniqueMessageSequences : List MessageRow → Prop
  | [] => True
  | row :: rest =>
      (∀ other, other ∈ rest → row.sequence ≠ other.sequence) ∧
        UniqueMessageSequences rest

def UniqueToolCallIds : List ToolCallRow → Prop
  | [] => True
  | row :: rest =>
      (∀ other, other ∈ rest → row.callId ≠ other.callId) ∧
        UniqueToolCallIds rest

def UniqueToolResultKeys : List MessageRow → Prop
  | [] => True
  | row :: rest =>
      (∀ key, row.isToolResultFor key = true →
        ∀ other, other ∈ rest → other.isToolResultFor key = false) ∧
        UniqueToolResultKeys rest

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

def MessageSequencesUnique (s : TranscriptState) : Prop :=
  UniqueMessageSequences s.messages

def ToolCallIdsUnique (s : TranscriptState) : Prop :=
  UniqueToolCallIds s.toolCalls

def ToolResultKeysUnique (s : TranscriptState) : Prop :=
  UniqueToolResultKeys s.messages

def OrderedBySequence (s : TranscriptState) : Prop :=
  StrictlyIncreasingMessages s.messages

def NextSeqAboveRows (s : TranscriptState) : Prop :=
  (∀ row, row ∈ s.messages → row.sequence < s.nextSeq) ∧
    (∀ call, call ∈ s.toolCalls → call.messageSequence < s.nextSeq)

def ReservedByPersistedMessage (s : TranscriptState) (call : ToolCallRow) : Prop :=
  ∃ row, row ∈ s.messages ∧
    row.reservesToolCall call.callId call.sessionId call.messageSequence

def ReservedByAssistantTurn (s : TranscriptState) (call : ToolCallRow) : Prop :=
  ∃ turn, s.assistantTurn = some turn ∧ turn.reservesToolCall call

def ToolCallReservedByMessage (s : TranscriptState) : Prop :=
  ∀ call, call ∈ s.toolCalls →
    ReservedByPersistedMessage s call ∨ ReservedByAssistantTurn s call

def CompletedToolCallsPaired (s : TranscriptState) : Prop :=
  ∀ call, call ∈ s.toolCalls →
    call.state = .completed →
      ∀ key, call.resultKey = some key →
        s.toolResultMessageCount key = 1

def ToolResultMessagesPaired (s : TranscriptState) : Prop :=
  ∀ row, row ∈ s.messages →
    ∀ callId key, row.kind = .toolResult callId key →
      ∃ call, call ∈ s.toolCalls ∧
        call.callId = callId ∧
        call.state = .completed ∧
        call.resultKey = some key

def PairClosed (s : TranscriptState) : Prop :=
  s.ToolCallReservedByMessage ∧
    s.CompletedToolCallsPaired ∧
    s.ToolResultMessagesPaired

/-- "Strong drain" is the stronger hook-drop property that current Rust does
not implement: no durable row remains running. -/
def StrongDrain (s : TranscriptState) : Prop :=
  ∀ call, call ∈ s.toolCalls → call.state ≠ .running

structure Coherent (s : TranscriptState) : Prop where
  ordered : s.OrderedBySequence
  messageSequencesUnique : s.MessageSequencesUnique
  toolCallIdsUnique : s.ToolCallIdsUnique
  toolResultKeysUnique : s.ToolResultKeysUnique
  nextSeqAboveRows : s.NextSeqAboveRows
  toolCallReservedByMessage : s.ToolCallReservedByMessage
  pairClosed : s.PairClosed

def RetainsPairs (_pre post : TranscriptState) : Prop :=
  post.PairClosed ∧ post.OrderedBySequence

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
    assistantTurn := none
  }

def beginAssistantToolCall (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) : TranscriptState :=
  let sequence :=
    match s.assistantTurn with
    | some turn => turn.sequence
    | none => s.nextSeq
  let turn :=
    match s.assistantTurn with
    | some existing => { existing with callIds := insert callId existing.callIds }
    | none =>
        { sessionId := s.sessionId
        , sequence := sequence
        , callIds := insert callId ∅
        }
  { s with
    nextSeq := match s.assistantTurn with | some _ => s.nextSeq | none => s.nextSeq + 1
    toolCalls := s.toolCalls ++
      [{ sessionId := s.sessionId
       , callId := callId
       , messageSequence := sequence
       , state := .running
       , resultKey := none }]
    inFlight := insert callId s.inFlight
    assistantTurn := some turn
  }

def persistAssistantMessage (s : TranscriptState) (messageId : MessageId)
    (turn : AssistantTurn) : TranscriptState :=
  { s with
    messages := s.messages ++
      [{ messageId := messageId
       , sessionId := turn.sessionId
       , sequence := turn.sequence
       , role := .assistant
       , kind := .assistantToolCalls turn.callIds }]
    assistantTurn := none
  }

def completeToolWithResult (s : TranscriptState)
    (callId : ToolExecution.ToolCallId)
    (messageId : MessageId)
    (key : ToolResultKey) : TranscriptState :=
  { s with
    nextSeq := s.nextSeq + 1
    messages := s.messages ++
      [{ messageId := messageId
       , sessionId := s.sessionId
       , sequence := s.nextSeq
       , role := .user
       , kind := .toolResult callId key }]
    toolCalls := replaceToolCall s.toolCalls callId
      (fun row => { row with state := .completed, resultKey := some key })
    inFlight := s.inFlight.erase callId
    assistantTurn := none
  }

def terminalizeInFlight (s : TranscriptState)
    (callId : ToolExecution.ToolCallId)
    (terminal : ToolExecution.ToolCallState) : TranscriptState :=
  { s with
    toolCalls := replaceToolCall s.toolCalls callId
      (fun row => { row with state := terminal })
    inFlight := s.inFlight.erase callId
  }

def abandonHookOwnership (s : TranscriptState) : TranscriptState :=
  { s with inFlight := ∅ }

end TranscriptState

end Transcript
