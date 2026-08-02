import Proofs.Basic
import Proofs.Transcript.State
import Proofs.StreamingResponse.State
import Proofs.PromptAssembly.State

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

/-- Every row announcing tool calls carries the assistant role.

A structural fact the transcript writer maintains — `persistAssistantMessage`
sets `role := .assistant` alongside `kind := .assistantToolCalls`. Pair closure
needs it: `ActiveBlockValid` locates the *announcement* for a retained result,
and this is what makes that announcement an acceptable *caller*. -/
def AnnouncementsAreAssistant (msgs : List MessageRow) : Prop :=
  ∀ row, row ∈ msgs →
    ∀ callIds, row.kind = .assistantToolCalls callIds → row.role = .assistant

/-- Coherence of a prompt view.

`blockValid` and `announcementsAssistant` were added with the real `summarize`
reducer (#993). The runtime establishes both by construction — every view is
born from `providerView`, and `providerView_sound` gives `ProviderValid` — and
they are exactly the premises under which dropping a compacted prefix preserves
pair closure. The previous three fields sufficed only because the modelled
reducer was `id`. -/
structure ViewCoherent (v : PromptView) : Prop where
  pairs                  : PairsClosedInMessages v.messages
  ordered                : StrictlyIncreasingMessages v.messages
  uniqueSequences        : UniqueMessageSequences v.messages
  blockValid             : PromptAssembly.ActiveBlockValid v.messages
  announcementsAssistant : AnnouncementsAreAssistant v.messages

def safeToReduce (v : PromptView) : Prop :=
  ∀ row, row ∈ v.messages →
    (∃ callId key, row.kind = .toolResult callId key) →
      ∃ status, v.responseStatuses row.messageId = some status ∧
        isTerminal status

end PromptView

end Compaction
