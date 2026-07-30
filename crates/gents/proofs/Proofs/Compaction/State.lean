import Proofs.Basic
import Proofs.Transcript.State
import Proofs.StreamingResponse.State

namespace Compaction

open Transcript (Sequence MessageId MessageRow MessageKind ToolResultKey
                 MessageRole StrictlyIncreasingMessages UniqueMessageSequences)

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

structure ViewCoherent (v : PromptView) : Prop where
  pairs           : PairsClosedInMessages v.messages
  ordered         : StrictlyIncreasingMessages v.messages
  uniqueSequences : UniqueMessageSequences v.messages

def safeToReduce (v : PromptView) : Prop :=
  ∀ row, row ∈ v.messages →
    (∃ callId key, row.kind = .toolResult callId key) →
      ∃ status, v.responseStatuses row.messageId = some status ∧
        isTerminal status

end PromptView

end Compaction
