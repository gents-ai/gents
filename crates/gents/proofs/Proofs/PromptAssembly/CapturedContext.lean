import Proofs.Enrollment.CapturedContext
import Proofs.CanonicalOutput.Message

namespace PromptAssembly.CapturedContext

open Enrollment CanonicalOutput Lean

private def optionalJson {α : Type} (encode : α → Json) : Option α → Json
  | none => .null
  | some value => encode value

private def positionJson (position : CapturedPosition) : Json :=
  Json.mkObj [("line", toJson position.line), ("character", toJson position.character)]

private def rangeJson (range : CapturedRange) : Json :=
  Json.mkObj [("start", positionJson range.start), ("end", positionJson range.end)]

private def metadataFields (context : Enrollment.CapturedContext) : List (String × Json) :=
  [("kind", .str context.kind.toWireName),
   ("source", .str context.source.toWireName),
   ("capturedAt", .str context.capturedAt),
   ("uri", optionalJson Json.str context.uri),
   ("terminalName", optionalJson Json.str context.terminalName),
   ("workspaceUri", optionalJson Json.str context.workspaceUri),
   ("version", optionalJson toJson context.version),
   ("unsaved", .bool context.unsaved),
   ("range", optionalJson rangeJson context.range)]

def metadataJson (context : Enrollment.CapturedContext) : Json :=
  Json.mkObj (metadataFields context)

def contextJson (context : Enrollment.CapturedContext) : Json :=
  Json.mkObj (metadataFields context ++ [("content", .str context.content)])

/-- Captured bytes use the existing native document payload. Only observations
remain in its metadata. Immutable output references and the existing fork owner
retain both; no second transcript or live-file dereference is introduced. -/
def nativeDocument (context : Enrollment.CapturedContext) : Media (List UInt8) :=
  { kind := .document
    data := .string context.content.toUTF8.data.toList
    mediaType := some "txt"
    additionalParams := some (Json.mkObj [("captured_context", metadataJson context)]).compress }

private def prosePrefix (prose : String) (contexts : List Enrollment.CapturedContext) : List String :=
  if prose.isEmpty && !contexts.isEmpty then [] else [prose]

/-- Ordinary authored text is never classified by its contents. Empty prose
with attachments creates only document blocks; ordinary empty text retains its
existing native representation. Structural admission still owns validity. -/
def authoredMessage (prose : String) (contexts : List Enrollment.CapturedContext) :
    ReconstructedMessage :=
  { role := .user
    nativeId := none
    blocks := (prosePrefix prose contexts).map (fun text => .text text.toUTF8.data.toList) ++
      contexts.map (fun context => .media (nativeDocument context)) }

/-- The owned provider-input boundary formats each typed capture once. The
metadata marker belongs to native storage and is not a provider parameter. -/
def providerText (context : Enrollment.CapturedContext) : String :=
  "Editor context (captured text; URI identifies its editor location):\n" ++
    (contextJson context).compress

def providerParts (prose : String) (contexts : List Enrollment.CapturedContext) : List String :=
  prosePrefix prose contexts ++ contexts.map providerText

theorem document_payload_is_the_captured_snapshot (context : Enrollment.CapturedContext) :
    (nativeDocument context).data = .string context.content.toUTF8.data.toList := rfl

theorem ordinary_text_is_not_inferred_as_context (prose : String) :
    (authoredMessage prose []).blocks = [.text prose.toUTF8.data.toList] ∧
    providerParts prose [] = [prose] := by
  simp [authoredMessage, providerParts, prosePrefix]

theorem attachment_only_input_has_no_fabricated_prose
    (contexts : List Enrollment.CapturedContext) (h : contexts ≠ []) :
    (authoredMessage "" contexts).blocks =
      contexts.map (fun context => .media (nativeDocument context)) ∧
    providerParts "" contexts = contexts.map providerText := by
  have emptyProse : "".isEmpty = true := rfl
  simp [authoredMessage, providerParts, prosePrefix, List.isEmpty_iff, h, emptyProse]

theorem native_and_provider_part_counts_agree (prose : String)
    (contexts : List Enrollment.CapturedContext) :
    (authoredMessage prose contexts).blocks.length = (providerParts prose contexts).length := by
  simp [authoredMessage, providerParts]

end PromptAssembly.CapturedContext
