import Proofs.Enrollment.State

namespace Enrollment

inductive CapturedContextKind where
  | selection | file | diagnostics | terminal
  deriving DecidableEq, Repr

def CapturedContextKind.toWireName : CapturedContextKind → String
  | .selection => "selection"
  | .file => "file"
  | .diagnostics => "diagnostics"
  | .terminal => "terminal"

inductive CapturedContextSource where
  | editor | clipboard | composer
  deriving DecidableEq, Repr

def CapturedContextSource.toWireName : CapturedContextSource → String
  | .editor => "editor"
  | .clipboard => "clipboard"
  | .composer => "composer"

/-- Editor positions are zero-based UTF-16 code-unit observations. They are not
byte offsets into the captured UTF-8 text or authority to read a live file. -/
structure CapturedPosition where
  line : Nat
  character : Nat
  deriving DecidableEq, Repr

structure CapturedRange where
  start : CapturedPosition
  «end» : CapturedPosition
  deriving DecidableEq, Repr

def CapturedRange.wellFormed (range : CapturedRange) : Bool :=
  decide (range.start.line < range.end.line ∨
    (range.start.line = range.end.line ∧ range.start.character ≤ range.end.character))

/-- All fields are immutable caller-supplied observations, including URIs,
workspace labels, versions and unsaved state. Admission authenticates the
snapshot, not the truth of the observations. No field grants file or workspace
access, and execution must never reopen a URI to replace the captured bytes. -/
structure CapturedContext where
  kind : CapturedContextKind
  source : CapturedContextSource
  capturedAt : String
  uri : Option String := none
  terminalName : Option String := none
  workspaceUri : Option String := none
  version : Option Nat := none
  unsaved : Bool
  range : Option CapturedRange := none
  content : String
  deriving DecidableEq, Repr

def CapturedContext.wellFormed (context : CapturedContext) : Bool :=
  (context.version.map (fun version => decide (0 < version))).getD true &&
    (context.range.map CapturedRange.wellFormed).getD true &&
    (context.kind != .selection || context.range.isSome)

/-- Optional framing distinguishes missing observations from explicit empty
ones. The request receipt's existing field framing also binds Unicode bytes,
attachment order, duplicates and every part of the captured text. -/
private def observationFields {α : Type} (encode : α → List String) : Option α → List String
  | none => ["none"]
  | some value => "some" :: encode value

def capturedContextFields (context : CapturedContext) : List String :=
  [context.kind.toWireName, context.source.toWireName, context.capturedAt] ++
    observationFields (fun value => [value]) context.uri ++
    observationFields (fun value => [value]) context.terminalName ++
    observationFields (fun value => [value]) context.workspaceUri ++
    observationFields (fun value => [toString value]) context.version ++
    [toString context.unsaved] ++
    observationFields (fun range =>
      [toString range.start.line, toString range.start.character,
       toString range.end.line, toString range.end.character]) context.range ++
    [context.content]

/-- Empty input retains its existing receipt bytes. Nonempty captured context
extends the same canonical receipt; it is never inferred from authored prose. -/
def capturedContextExtensionFields (contexts : List CapturedContext) : List String :=
  if contexts.isEmpty then []
  else ["captured-context", toString contexts.length] ++ contexts.flatMap capturedContextFields

theorem empty_capture_has_no_encoding_extension :
    capturedContextExtensionFields [] = [] := rfl

theorem captured_uri_does_not_change_structure (context : CapturedContext)
    (uri workspaceUri : Option String) :
    ({context with uri, workspaceUri} : CapturedContext).wellFormed = context.wellFormed := rfl

end Enrollment
