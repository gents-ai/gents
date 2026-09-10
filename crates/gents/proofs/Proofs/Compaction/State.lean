import Proofs.Basic
import Proofs.Transcript.State
import Proofs.StreamingResponse.State
import Proofs.PromptAssembly.State

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
  responseStatuses : MessageId → Option StreamingResponse.Status

namespace PromptView

def PairsClosedInMessages (msgs : List MessageRow) : Prop :=
  ∀ row, row ∈ msgs →
    ∀ callId key, row.kind = .toolResult callId key →
      ∃ caller, caller ∈ msgs ∧
        caller.role = .assistant ∧
        (∃ callIds, caller.kind = .assistantToolCalls callIds ∧ callId ∈ callIds)

/-- Every row announcing tool calls carries the assistant role.

A structural fact the transcript writer maintains — `persistAssistantMessage`
sets `role := .assistant` alongside `kind := .assistantToolCalls`. Pair closure
needs it: `ActiveBlockValid` locates the *announcement* for a retained result,
and this is what makes that announcement an acceptable *caller*. -/
def AnnouncementsAreAssistant (msgs : List MessageRow) : Prop :=
  ∀ row, row ∈ msgs →
    ∀ callIds, row.kind = .assistantToolCalls callIds → row.role = .assistant

/-- Premises for safe reduction of a row-level prompt view. Provider validity
and assistant announcement roles are separate obligations. The global provider
proof supplies validity only under its unique-call-id premise; it does not
establish arbitrary production content or repeated-call behavior. -/
structure ViewCoherent (v : PromptView) : Prop where
  pairs                  : PairsClosedInMessages v.messages
  ordered                : StrictlyIncreasingMessages v.messages
  blockValid             : PromptAssembly.ActiveBlockValid v.messages
  announcementsAssistant : AnnouncementsAreAssistant v.messages

/-- Only tool-result rows require a terminal response observation. The finite
row traversal keeps the actual reduction gate executable. -/
def safeToReduce (v : PromptView) : Prop :=
  ∀ row ∈ v.messages,
    match row.kind with
    | .toolResult _ _ =>
        match v.responseStatuses row.messageId with
        | some status => isTerminal status
        | none => False
    | _ => True

instance (v : PromptView) : Decidable (safeToReduce v) := by
  unfold safeToReduce
  have rowDec : ∀ row : MessageRow, Decidable
      (match row.kind with
       | .toolResult _ _ => match v.responseStatuses row.messageId with
         | some status => isTerminal status
         | none => False
       | _ => True) := by
    intro row
    cases row.kind <;> try infer_instance
    cases v.responseStatuses row.messageId <;> infer_instance
  exact @List.decidableBAll _ _ rowDec v.messages

end PromptView

end Compaction
