import Proofs.CanonicalOutput.Hydration
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.Contracts.TerminalSelection
open CanonicalOutput

def selectionJson : Option CanonicalOutput.TerminalSelection → String
  | none => "null"
  | some .noMessage => "{\"kind\":\"no_message\"}"
  | some (.message id) => "{\"kind\":\"message\",\"message_doc_id\":\"message-" ++ toString id ++ "\"}"

def caseJson (state : RequestState) (selection : Option CanonicalOutput.TerminalSelection) : String :=
  let expected := observeTerminalSelection state selection
  let admitted := if isTerminal state then
      match Hydration.terminalRoots 2 [] []
          [{request := 1, session := 2, selection := selection, state := state}] with
      | .ok _ => true
      | .error _ => false
    else true
  "{\"state\":" ++ jsonString state.toDefraDB ++
    ",\"selection\":" ++ selectionJson selection ++
    ",\"expected\":" ++ selectionJson expected ++
    ",\"hydration_admitted\":" ++ jsonOptionalBool (some admitted) ++ "}"

def casesJson : String :=
  jsonArray ([RequestState.workspaceBindingPending, .pending, .claimed, .processing,
      .completed, .failed, .superseded, .dead, .interrupted].flatMap fun state =>
    [none, some .noMessage, some (.message 7)].map (caseJson state))

end Conformance.Contracts.TerminalSelection
