import Proofs.Shared.ToolResultText

/-!
# Tool results on the provider path

A tool's output is a list of parts. The owned loop threads each part into the
next provider request: text is bounded by the tool-result text limit, an image
travels whole to a provider whose input carries tool-result images and
otherwise becomes a note naming the omission. A result loaded back from the
session is replayed as one text part: an image is never replayed, whatever its
size, and leaves a note asking the model to call the tool again; a text-only
result has no replayed projection here.

Every request's tool results then take the shape `ToolResultText.wire`
chooses for their text, so a result whose text parts would be cut on some
route arrives as one wrapped part holding their segments in order.

Rust owner: `gents_loop::loop_stream::turn_threading` (`bounded_tool_result`,
`replayed_tool_result_text`), then `gents_loop::provider_input::tool_result_text`.
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

/-- What a text part holds, as `ToolResultText.wire` reads it: no `{`, exactly
one JSON object or array, or braces in anything else. -/
inductive TextShape where
  | plain
  | json
  | mixed
  deriving DecidableEq, Repr

inductive SourcePart where
  | text (withinBound : Bool) (shape : TextShape)
  | image
  deriving DecidableEq, Repr

/-- What one part of the result is when it reaches the provider. -/
inductive Segment where
  | textWhole
  | textBounded
  | imageWhole
  | imageOmittedNote
  | imageNotReplayedNote
  /-- Opens a part whose text is the JSON object `{"text": …}`; the segments
  after it are the text it holds, in order. -/
  | wrapped
  | other
  deriving DecidableEq, Repr

def threadText (withinBound : Bool) : Segment :=
  if withinBound then .textWhole else .textBounded

def threadPart (profile : Profile) : SourcePart → Segment
  | .text withinBound _ => threadText withinBound
  | .image => if carriesImages profile then .imageWhole else .imageOmittedNote

/-- A bounded prefix of JSON is no longer one JSON value; the notes have no
braces. -/
def threadShape (profile : Profile) : SourcePart → Option TextShape
  | .text true shape => some shape
  | .text false .plain => some .plain
  | .text false _ => some .mixed
  | .image => if carriesImages profile then none else some .plain

def partsOf : Nat → ToolResultText.Parts
  | 0 => .none
  | 1 => .one
  | _ => .many

/-- The text parts merged into one wrapped part at the first text part's place;
other parts stay where they were. -/
def mergeTexts (merged : List Segment) : Bool → List (List Segment × Option TextShape) → List (List Segment)
  | _, [] => []
  | first, (segments, none) :: rest => segments :: mergeTexts merged first rest
  | true, (_, some _) :: rest => merged :: mergeTexts merged false rest
  | false, (_, some _) :: rest => mergeTexts merged false rest

/-- One tool result as the provider receives it: each part's segments, after
`ToolResultText.wire` shapes its text parts. -/
def forProvider (parts : List (List Segment × Option TextShape)) : List (List Segment) :=
  let shapes := parts.filterMap (·.2)
  match ToolResultText.wire (partsOf shapes.length) (shapes.any (· != .plain)) (shapes == [.json]) with
  | .asIs => parts.map (·.1)
  | .wrapped => mergeTexts (.wrapped :: (parts.filter (·.2.isSome)).flatMap (·.1)) true parts

/-- Each part of a fresh result is one part of the next request. -/
def threaded (profile : Profile) (parts : List SourcePart) : List (List Segment) :=
  forProvider (parts.map fun part => ([threadPart profile part], threadShape profile part))

def hasImage (parts : List SourcePart) : Bool :=
  parts.any fun part => part == .image

def replayPart : SourcePart → Segment
  | .text withinBound _ => threadText withinBound
  | .image => .imageNotReplayedNote

/-- The replayed text joins the parts with newlines, so it is one JSON value
only if it is exactly one whole JSON part. -/
def replayShape (parts : List SourcePart) : TextShape :=
  if parts.all (fun | .text _ .plain | .image => true | .text _ _ => false) then .plain
  else if parts == [.text true .json] then .json
  else .mixed

/-- A persisted result with an image is loaded back as one text part, its
segments in order. -/
def replayedParts (parts : List SourcePart) : Option (List (List Segment)) :=
  if hasImage parts then some [parts.map replayPart] else none

/-- The loaded-back result as the next request carries it. -/
def replayed (parts : List SourcePart) : Option (List (List Segment)) :=
  if hasImage parts then some (forProvider [(parts.map replayPart, some (replayShape parts))])
  else none

theorem an_image_reaches_an_image_capable_provider_whole (profile : Profile)
    (h : carriesImages profile = true) : threadPart profile .image = .imageWhole := by
  simp [threadPart, h]

theorem an_image_becomes_a_note_where_the_provider_refuses_it (profile : Profile)
    (h : carriesImages profile = false) : threadPart profile .image = .imageOmittedNote := by
  simp [threadPart, h]

theorem an_oversized_text_result_is_bounded_before_threading (profile : Profile) (shape : TextShape) :
    threadPart profile (.text false shape) = .textBounded := rfl

theorem a_replayed_result_never_carries_an_image (parts : List SourcePart)
    (projection : List (List Segment)) (h : replayedParts parts = some projection) :
    ∀ part ∈ projection, .imageWhole ∉ part := by
  unfold replayedParts at h
  split at h
  · cases h
    intro part hpart
    simp only [List.mem_singleton] at hpart
    subst hpart
    intro hmem
    obtain ⟨source, _, hsource⟩ := List.mem_map.mp hmem
    cases source with
    | text withinBound _ => cases withinBound <;> simp [replayPart, threadText] at hsource
    | image => simp [replayPart] at hsource
  · cases h

/-- A JSON response beside an image a provider refuses reaches it as one
wrapped part, so the omission note cannot cost the response. -/
theorem a_json_response_and_an_omission_note_arrive_wrapped :
    threaded .openAiChatCompletions [.text true .json, .image] =
      [[.wrapped, .textWhole, .imageOmittedNote]] := by decide

theorem a_json_response_beside_a_carried_image_is_sent_as_it_is :
    threaded .claudeMessages [.text true .json, .image] = [[.textWhole], [.imageWhole]] := by decide

theorem plain_text_is_threaded_part_for_part (profile : Profile) (withinBound : Bool) :
    threaded profile [.text withinBound .plain] = [[threadText withinBound]] := by
  cases profile <;> cases withinBound <;> rfl

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
