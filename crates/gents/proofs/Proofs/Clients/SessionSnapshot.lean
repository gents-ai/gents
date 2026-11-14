import Proofs.Session.Transcript.State
import Proofs.Clients.ClientShell.Timeline
import Mathlib.Data.List.Dedup

/-!
Session snapshot projection: what a client's session snapshot shows of the transcript rows it
reads. Every message row and every tool-call row is shown, and the tool calls are shown as one
tool group per message sequence, placed by the client timeline order
(`ClientShell.Timeline.buildOrder`) over the group sequences in ascending order.
-/

namespace SessionSnapshot

open Transcript

structure MessageEntry where
  sequence : Sequence
  role : MessageRole
  deriving DecidableEq, Repr

structure ToolCallEntry where
  callId : ToolExecution.ToolCallId
  messageSequence : Sequence
  state : ToolExecution.ToolCallState
  deriving DecidableEq, Repr

structure ToolGroup where
  messageSequence : Sequence
  tools : Nat
  deriving DecidableEq, Repr

structure Observation where
  messageCount : Nat
  toolCallCount : Nat
  toolGroups : List ToolGroup
  deriving DecidableEq, Repr

def messageEntries (s : TranscriptState) : List MessageEntry :=
  s.messages.map fun row => { sequence := row.sequence, role := row.role }

def toolCallEntries (s : TranscriptState) : List ToolCallEntry :=
  s.toolCalls.map fun row =>
    { callId := row.callId, messageSequence := row.messageSequence, state := row.state }

def timelineRole : MessageRole → ClientShell.Timeline.Role
  | .user => .user
  | .assistant => .assistant

def timelineMessages (messages : List MessageEntry) : List ClientShell.Timeline.Msg :=
  ((List.range messages.length).zip messages).map fun (key, message) =>
    { key, seq := message.sequence, role := timelineRole message.role, emitsItem := true
      token := none }

def groupSequences (calls : List ToolCallEntry) : List Int :=
  (calls.map fun call => (call.messageSequence : Int)).dedup.mergeSort
    (fun left right => decide (left ≤ right))

def toolsAt (calls : List ToolCallEntry) (sequence : Int) : Nat :=
  (calls.filter fun call => (call.messageSequence : Int) == sequence).length

def groupSlot? : ClientShell.Timeline.Slot → Option Int
  | .toolGroup sequence => some sequence
  | _ => none

def toolGroupSequences (messages : List MessageEntry) (calls : List ToolCallEntry) :
    List Int :=
  (ClientShell.Timeline.buildOrder (groupSequences calls) (timelineMessages messages) []
    none).filterMap groupSlot?

def project (messages : List MessageEntry) (calls : List ToolCallEntry) : Observation :=
  { messageCount := messages.length
    toolCallCount := calls.length
    toolGroups := (toolGroupSequences messages calls).map fun sequence =>
      { messageSequence := sequence.toNat, tools := toolsAt calls sequence } }

def ofTranscript (s : TranscriptState) : Observation :=
  project (messageEntries s) (toolCallEntries s)

theorem every_row_is_shown (messages : List MessageEntry) (calls : List ToolCallEntry) :
    (project messages calls).messageCount = messages.length ∧
      (project messages calls).toolCallCount = calls.length :=
  ⟨rfl, rfl⟩

theorem count_groupSlot (slots : List ClientShell.Timeline.Slot) (sequence : Int) :
    (slots.filterMap groupSlot?).count sequence =
      slots.count (ClientShell.Timeline.Slot.toolGroup sequence) := by
  induction slots with
  | nil => rfl
  | cons slot rest ih =>
    cases slot <;> simp [groupSlot?, List.filterMap_cons, List.count_cons, ih]

theorem groupSequences_nodup (calls : List ToolCallEntry) : (groupSequences calls).Nodup :=
  (List.mergeSort_perm _ _).nodup_iff.mpr (List.nodup_dedup _)

theorem mem_groupSequences (calls : List ToolCallEntry) (sequence : Int) :
    sequence ∈ groupSequences calls ↔
      ∃ call ∈ calls, (call.messageSequence : Int) = sequence := by
  simp [groupSequences, (List.mergeSort_perm _ _).mem_iff, List.mem_dedup]

theorem one_tool_group_per_sequence (messages : List MessageEntry)
    (calls : List ToolCallEntry) (sequence : Int) :
    (toolGroupSequences messages calls).count sequence =
      if ∃ call ∈ calls, (call.messageSequence : Int) = sequence then 1 else 0 := by
  rw [toolGroupSequences, count_groupSlot,
    ClientShell.Timeline.tool_group_emitted_once _ (groupSequences_nodup calls)]
  simp only [mem_groupSequences]

end SessionSnapshot
