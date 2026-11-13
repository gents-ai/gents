/-!
# Tool results on the provider path

A tool's output is a list of parts. The owned loop threads each part into the
next provider request: text is bounded by the tool-result text limit, an image
travels whole to a provider whose input carries tool-result images and
otherwise becomes a note naming the omission. A result loaded back from the
session is replayed as one text part: an image is never replayed, whatever its
size, and leaves a note asking the model to call the tool again; a text-only
result has no replayed projection here.

Rust owner: `gents_loop::loop_stream::turn_threading` (`bounded_tool_result`,
`replayed_tool_result_text`).
-/

namespace ToolResultThreading

inductive Profile where
  | openAiChatCompletions
  | openAiResponsesNormalized
  | openRouterChatCompletions
  | chatGptCodexResponses
  | xaiResponses
  | claudeMessages
  deriving DecidableEq, Repr

/-- Only the Claude Messages wire accepts image blocks inside a tool result. -/
def carriesImages : Profile → Bool
  | .claudeMessages => true
  | _ => false

inductive SourcePart where
  | text (withinBound : Bool)
  | image
  deriving DecidableEq, Repr

/-- What one part of the result is when it reaches the provider. -/
inductive Segment where
  | textWhole
  | textBounded
  | imageWhole
  | imageOmittedNote
  | imageNotReplayedNote
  | other
  deriving DecidableEq, Repr

def threadText (withinBound : Bool) : Segment :=
  if withinBound then .textWhole else .textBounded

def threadPart (profile : Profile) : SourcePart → Segment
  | .text withinBound => threadText withinBound
  | .image => if carriesImages profile then .imageWhole else .imageOmittedNote

/-- Each part of a fresh result is one part of the next request. -/
def threaded (profile : Profile) (parts : List SourcePart) : List (List Segment) :=
  parts.map fun part => [threadPart profile part]

def hasImage (parts : List SourcePart) : Bool :=
  parts.any fun part => part == .image

def replayPart : SourcePart → Segment
  | .text withinBound => threadText withinBound
  | .image => .imageNotReplayedNote

/-- A persisted result with an image is replayed as one text part, its
segments in order. -/
def replayed (parts : List SourcePart) : Option (List (List Segment)) :=
  if hasImage parts then some [parts.map replayPart] else none

theorem an_image_reaches_an_image_capable_provider_whole (profile : Profile)
    (h : carriesImages profile = true) : threadPart profile .image = .imageWhole := by
  simp [threadPart, h]

theorem an_image_becomes_a_note_where_the_provider_refuses_it (profile : Profile)
    (h : carriesImages profile = false) : threadPart profile .image = .imageOmittedNote := by
  simp [threadPart, h]

theorem an_oversized_text_result_is_bounded_before_threading (profile : Profile) :
    threadPart profile (.text false) = .textBounded := rfl

theorem a_replayed_result_never_carries_an_image (parts : List SourcePart)
    (projection : List (List Segment)) (h : replayed parts = some projection) :
    ∀ part ∈ projection, .imageWhole ∉ part := by
  unfold replayed at h
  split at h
  · cases h
    intro part hpart
    simp only [List.mem_singleton] at hpart
    subst hpart
    intro hmem
    obtain ⟨source, _, hsource⟩ := List.mem_map.mp hmem
    cases source with
    | text withinBound => cases withinBound <;> simp [replayPart, threadText] at hsource
    | image => simp [replayPart] at hsource
  · cases h

/-- What a later request carries as the id of an earlier assistant turn. -/
inductive AssistantId where
  | providerId
  | absent
  | other
  deriving DecidableEq, Repr

/-- The owned loop stamps each accepted assistant turn with the message id its
provider stream reported, and threads that turn into every later request of the
execution: after `toolTurns` tool-calling turns there are `toolTurns` later
requests, the `k`-th carrying the first `k` assistant turns in order. A
Responses follow-up references the provider message id, so dropping it breaks
the provider's own threading. -/
def threadedAssistantIds (reportsId : Bool) (toolTurns : Nat) : List (List AssistantId) :=
  (List.range toolTurns).map fun turn =>
    List.replicate (turn + 1) (if reportsId then .providerId else .absent)

theorem threaded_assistant_turn_carries_provider_message_id (toolTurns : Nat) :
    ∀ request ∈ threadedAssistantIds true toolTurns, ∀ id ∈ request, id = .providerId := by
  intro request hrequest id hid
  obtain ⟨turn, _, rfl⟩ := List.mem_map.mp hrequest
  simpa using (List.mem_replicate.mp hid).2

theorem every_tool_turn_is_followed_by_a_request (reportsId : Bool) (toolTurns : Nat) :
    (threadedAssistantIds reportsId toolTurns).length = toolTurns := by
  simp [threadedAssistantIds]

end ToolResultThreading
