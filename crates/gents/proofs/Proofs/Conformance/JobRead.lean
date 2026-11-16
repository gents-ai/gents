import Proofs.ToolCalls.PluginJob.Adoption
import Lean

open Lean PluginJob

def stateName : JobState → String
  | .running => "running"
  | .paused => "paused"
  | .awaitingUser => "awaiting_user"
  | .completed => "completed"
  | .failed => "failed"
  | .cancelled => "cancelled"
  | .timedOut => "timed_out"

def selectorJson : Except ReadSelectorError ReadSelector → Json
  | .ok (.address session tool) => Json.mkObj [("kind", toJson "address"),
      ("session_id", toJson session), ("tool_call_id", toJson tool)]
  | .ok (.row row) => Json.mkObj [("kind", toJson "row"), ("row_doc_id", toJson row)]
  | .error .invalidAddress => Json.mkObj [("kind", toJson "invalid_address")]
  | .error .invalidSelector => Json.mkObj [("kind", toJson "invalid_selector")]

def selectorCases : List Json :=
  let addresses := [none, some "", some "sess:call", some "sess:a:call", some "no-separator"]
  let rows := [none, some "", some "row-id"]
  addresses.flatMap fun address => rows.map fun row =>
    Json.mkObj [("address", toJson address), ("row_doc_id", toJson row),
      ("expected", selectorJson (selectRead address row))]

def filterCases : List Json :=
  let inputs : List (Option (List JobState)) :=
    [none, some [], some [.paused], some [.running, .completed]]
  inputs.map fun states => Json.mkObj [
    ("states", toJson (states.map (List.map stateName))),
    ("expected", toJson ((readStates states).map stateName))]

def main : IO Unit := IO.println (Json.mkObj [
  ("selectors", toJson selectorCases), ("filters", toJson filterCases)]).compress
