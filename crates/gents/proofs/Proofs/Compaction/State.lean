import Proofs.Basic
import Proofs.Transcript.State
import Proofs.StreamingResponse.State
import Proofs.PromptAssembly.Executable
import Mathlib.Logic.Equiv.List

namespace Compaction

open Transcript (Sequence MessageId MessageRow MessageKind ToolResultKey
                 MessageRole StrictlyIncreasingMessages)

structure SummaryHandle where
  payload : Nat
  deriving DecidableEq, Repr

structure PromptView where
  sessionId        : SessionId
  messages         : List MessageRow
  summary          : Option SummaryHandle
  outputObservations : MessageId → Option StreamingResponse.Observation

namespace PromptView

def PairsClosedInMessages (msgs : List MessageRow) : Prop :=
  ∀ row, row ∈ msgs →
    ∀ callId key, row.kind = .toolResult callId key →
      ∃ caller, caller ∈ msgs ∧
        caller.role = .assistant ∧
        (∃ callIds, caller.kind = .assistantToolCalls callIds ∧ callId ∈ callIds)

def AnnouncementsAreAssistant (msgs : List MessageRow) : Prop :=
  ∀ row, row ∈ msgs →
    ∀ callIds, row.kind = .assistantToolCalls callIds → row.role = .assistant

structure ViewCoherent (v : PromptView) : Prop where
  pairs                  : PairsClosedInMessages v.messages
  ordered                : StrictlyIncreasingMessages v.messages
  blockValid             : PromptAssembly.ActiveBlockValid v.messages
  announcementsAssistant : AnnouncementsAreAssistant v.messages

def endsAtTurnBoundary (messages : List MessageRow) : Bool :=
  match messages.getLast? with
  | some row => row.kind == .ordinary
  | none => false

def encodeProviderKey (key : String) : Nat :=
  Encodable.encode (key.toList.map Char.toNat)

theorem charToNat_injective : Function.Injective Char.toNat := by
  intro left right h
  rw [← Char.ofNat_toNat left, ← Char.ofNat_toNat right, h]

theorem encodeProviderKey_injective : Function.Injective encodeProviderKey := by
  intro left right h
  apply String.ext
  apply (List.map_injective_iff.mpr charToNat_injective)
  exact Encodable.encode_injective h

def providerCallSymbol (id : String) (callId : Option String) : Nat :=
  encodeProviderKey (callId.getD id)

def nativeToolCallSymbols (native : CanonicalOutput.ReconstructedMessage) : List Nat :=
  native.blocks.filterMap fun block => match block with
    | .toolCall _ id callId _ _ _ _ => some (providerCallSymbol id callId)
    | _ => none

def nativeToolResultFacts (native : CanonicalOutput.ReconstructedMessage) :
    List (CanonicalOutput.DocId × Nat) :=
  native.blocks.filterMap fun block => match block with
    | .toolResult physicalCall id callId _ =>
        some (physicalCall, providerCallSymbol id callId)
    | _ => none

def roleMatches (row : MessageRow) (message : CanonicalOutput.MessageEnvelope)
    (native : CanonicalOutput.ReconstructedMessage) : Bool :=
  match row.role, message.header.role, native.role with
  | .assistant, .assistant, .assistant | .user, .user, .user => true
  | _, _, _ => false

def resultPublicationMatches (publication : CanonicalOutput.MessagePublication)
    (physical : CanonicalOutput.DocId) : Bool :=
  match publication with
  | .toolDelivery call => call == physical
  | .fork _ => true
  | _ => false

def kindMatches (row : MessageRow) (message : CanonicalOutput.MessageEnvelope)
    (native : CanonicalOutput.ReconstructedMessage) : Bool :=
  let calls := nativeToolCallSymbols native
  let results := nativeToolResultFacts native
  match row.kind with
  | .ordinary => calls.isEmpty && results.isEmpty
  | .assistantToolCalls callIds =>
      row.role == .assistant && !calls.isEmpty && decide calls.Nodup &&
        calls.toFinset == callIds && results.isEmpty
  | .toolResult call key =>
      row.role == .user && key.sessionId == row.sessionId && calls.isEmpty &&
        match native.blocks with
        | [.toolResult physicalCall id callId _] =>
            providerCallSymbol id callId == call &&
              resultPublicationMatches message.header.publication physicalCall
        | _ => false

def rowPublished (v : PromptView) (row : MessageRow) : Bool :=
  match v.outputObservations row.messageId with
  | some observation => match StreamingResponse.project observation with
    | .published message native =>
        message.header.id == row.messageId &&
          row.sessionId == v.sessionId && message.header.session == row.sessionId &&
          message.sequence == row.sequence && roleMatches row message native &&
          kindMatches row message native
    | _ => false
  | _ => false

/-- Native session reads reconstruct published canonical messages. A closed turn
makes their provider projection stable under later turns even when older rows
need pairing repair. Persistence still requires an exact raw sequence cursor. -/
def canCompactProjection (v : PromptView) : Prop :=
  endsAtTurnBoundary v.messages = true ∧ v.messages.all (rowPublished v) = true

instance (v : PromptView) : Decidable (canCompactProjection v) := by
  unfold canCompactProjection
  infer_instance

def safeToReduce (v : PromptView) : Prop :=
  endsAtTurnBoundary v.messages = true ∧
    PromptAssembly.sanitizeTurn v.messages = v.messages ∧
    v.messages.all (rowPublished v) = true

instance (v : PromptView) : Decidable (safeToReduce v) := by
  unfold safeToReduce
  infer_instance

end PromptView

end Compaction
