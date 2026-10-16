import Proofs.PromptAssembly.ToolArgumentFailure
import Lean

namespace Conformance.ResponsesTermination
open Lean PromptAssembly.ToolArgumentFailure

private def eventTypes : List String :=
  ["response.completed", "response.incomplete", "response.failed", "response.output_text.delta"]

private def reasons : List (Option String) :=
  [none, some "max_output_tokens", some "content_filter", some "max_messages", some "steered", some "unknown"]

private def terminationName : Termination → String
  | .unreported => "unreported"
  | .completed => "completed"
  | .tokenLimit => "token_limit"

private def caseJson (delimited : Bool) (eventType : String) (reason : Option String) : Json :=
  Json.mkObj [("delimited", toJson delimited), ("event_type", toJson eventType),
    ("reason", toJson reason),
    ("termination", toJson ((responsesTermination delimited eventType reason).map terminationName))]

def casesJson : String :=
  (toJson ([false, true].flatMap fun delimited =>
    eventTypes.flatMap fun eventType => reasons.map (caseJson delimited eventType))).compress

end Conformance.ResponsesTermination

def main : IO Unit := IO.println Conformance.ResponsesTermination.casesJson
