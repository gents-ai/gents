import Proofs.Session.CanonicalOutput.Hydration
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.Contracts.TerminalSelection
open Lean CanonicalOutput

structure Selection where
  kind : String
  messageDocId : Option String
  deriving ToJson

def Selection.ofModel : CanonicalOutput.TerminalSelection → Selection
  | .noMessage => ⟨"no_message", none⟩
  | .message id => ⟨"message", some ("message-" ++ toString id)⟩

def Selection.toModel (selection : Selection) : CanonicalOutput.TerminalSelection :=
  match selection.messageDocId with
  | some doc => .message (doc.drop "message-".length).toNat!
  | none => .noMessage

structure Input where
  state : RequestState
  selection : Option Selection
  deriving ToJson

def inputs : List Input :=
  [RequestState.workspaceBindingPending, .pending, .claimed, .processing,
      .completed, .failed, .superseded, .dead, .interrupted].flatMap fun state =>
    [none, some .noMessage, some (.message 7)].map fun selection =>
      { state, selection := selection.map Selection.ofModel }

/-- The selection a request row reports: `AgentRequestRow::observed_terminal_output`. -/
@[conformance_family]
def observed : Family Input (Option Selection) where
  inputs := inputs
  model := fun i =>
    (observeTerminalSelection i.state (i.selection.map Selection.toModel)).map Selection.ofModel

/-- Whether session hydration admits the request as a terminal root: the file
inventory's session selection over a served node. -/
@[conformance_family]
def hydration : Family Input Bool where
  inputs := inputs
  model := fun i =>
    if isTerminal i.state then
      match Hydration.terminalRoots 2 [] []
          [{request := 1, session := 2, selection := i.selection.map Selection.toModel, state := i.state}] with
      | .ok _ => true
      | .error _ => false
    else true

end Conformance.Contracts.TerminalSelection
