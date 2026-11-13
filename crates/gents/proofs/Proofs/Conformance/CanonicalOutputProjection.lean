import Proofs.Conformance.CanonicalOutputWire
import Proofs.Session.StreamingResponse.Executable

namespace Conformance.CanonicalOutputProjection

open Lean StreamingResponse

structure ViewInput where
  name : String
  input : Json
  deriving ToJson

def caseNamed (name : String) : Option OutputProjectionCase :=
  outputProjectionCases.find? (·.name == name)

theorem case_names_are_unique :
    (outputProjectionCases.map (·.name)).eraseDups.length = outputProjectionCases.length := by
  native_decide

@[conformance_family]
def view : Family ViewInput Json where
  inputs := outputProjectionCases.map fun witness =>
    { name := witness.name, input := CanonicalOutputWire.observation witness.input }
  model := fun i => ((caseNamed i.name).map fun witness =>
    CanonicalOutputWire.view (project witness.input)).getD Json.null

structure RenderedInput where
  name : String
  native : Json
  deriving ToJson

def partKind : CanonicalOutput.ReasoningPart (List UInt8) → Option String
  | .text _ _ => some "reasoning"
  | .summary _ => some "summary"
  | .encrypted _ | .redacted _ => none

def partBytes : CanonicalOutput.ReasoningPart (List UInt8) → List UInt8
  | .text payload _ | .summary payload | .encrypted payload | .redacted payload => payload

/-- The rendered reasoning text pieces in order: each part whose kind is the
next of the published view's `renderedKinds`. -/
def renderedPieces (kinds : List String) :
    List (CanonicalOutput.ReasoningPart (List UInt8)) → List String
  | [] => []
  | part :: rest =>
      match partKind part, kinds with
      | some kind, next :: later =>
          if kind == next then
            (String.fromUTF8? (ByteArray.mk (partBytes part).toArray)).getD "" ::
              renderedPieces later rest
          else renderedPieces kinds rest
      | _, _ => renderedPieces kinds rest

def published : View → Option CanonicalOutput.ReconstructedMessage
  | .published _ native => some native
  | _ => none

def rendered (native : CanonicalOutput.ReconstructedMessage) (kinds : List String) : List String :=
  native.blocks.flatMap fun block => match block with
    | .reasoning _ parts => renderedPieces kinds parts
    | _ => []

/-- A rendering consumer presents one published native assistant message whose
only block is reasoning; the mixed and all-opaque reasoning cases are the
published views of that shape. -/
def renderedCases : List OutputProjectionCase :=
  outputProjectionCases.filter fun witness => witness.name ∈
    ["published_mixed_reasoning_renders_text_and_summary_only",
     "published_all_opaque_reasoning_renders_nothing"]

def renderedInputs : List RenderedInput :=
  renderedCases.filterMap fun witness => (published witness.expected).map fun native =>
    { name := witness.name, native := CanonicalOutputWire.reconstructed native }

def renderedOutput (i : RenderedInput) : List String :=
  ((caseNamed i.name).bind fun witness => (published witness.expected).map fun native =>
    rendered native (renderedKinds witness.expected)).getD []

/-- `present_message` renders a published reasoning message for persistence. -/
@[conformance_family]
def presentation : Family RenderedInput (List String) where
  inputs := renderedInputs
  model := renderedOutput

/-- `render_reasoning_text` renders the same message for the live preview. -/
@[conformance_family]
def preview : Family RenderedInput (List String) where
  inputs := renderedInputs
  model := renderedOutput

example : renderedInputs.length = 2 := by native_decide

example : renderedCases.all fun witness =>
    witness.input.messages.any fun candidate => match witness.expected with
      | .published message _ => candidate == message
      | _ => false := by
  native_decide

def opaqueVariants (native : CanonicalOutput.ReconstructedMessage) : Bool :=
  native.role == .assistant && match native.blocks with
    | [.reasoning _ parts] =>
        parts.any (fun part => match part with | .encrypted _ => true | _ => false) &&
        parts.any (fun part => match part with | .redacted _ => true | _ => false)
    | _ => false

example : renderedCases.all fun witness =>
    ((published witness.expected).map opaqueVariants).getD false := by
  native_decide

example : renderedCases.map (fun witness => (renderedKinds witness.expected).isEmpty) =
    [false, true] := by
  native_decide

example : renderedCases.all fun witness =>
    ((published witness.expected).map fun native =>
      (rendered native (renderedKinds witness.expected)).length ==
        (renderedKinds witness.expected).length).getD false := by
  native_decide

end Conformance.CanonicalOutputProjection
