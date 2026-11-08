import Lean
import Proofs.ClientShell.PackWatch

namespace Conformance.ClientPackWatch
open Lean ClientShell.PackWatch

def reasonOf : String → Except String TerminalReason
  | "auth" => pure .auth
  | "permanent" => pure .permanent
  | other => throw s!"unknown terminal reason {other}"

def reasonJson : TerminalReason → Json
  | .auth => "auth"
  | .permanent => "permanent"

def phaseOf (json : Json) : Except String Phase := do
  match ← json.getObjValAs? String "kind" with
  | "starting" => pure (.starting (← json.getObjValAs? Nat "failures"))
  | "streaming" => pure .streaming
  | "backoff" => pure (.backoff (← json.getObjValAs? Nat "failures"))
  | "terminal" => pure (.terminal (← reasonOf (← json.getObjValAs? String "reason")))
  | "cancelled" => pure .cancelled
  | other => throw s!"unknown phase {other}"

def phaseJson : Phase → Json
  | .starting failures => Json.mkObj [("kind", "starting"), ("failures", toJson failures)]
  | .streaming => Json.mkObj [("kind", "streaming")]
  | .backoff failures => Json.mkObj [("kind", "backoff"), ("failures", toJson failures)]
  | .terminal reason => Json.mkObj [("kind", "terminal"), ("reason", reasonJson reason)]
  | .cancelled => Json.mkObj [("kind", "cancelled")]

def eventOf : String → Except String Event
  | "connected" => pure .connected
  | "cleanEnd" => pure .cleanEnd
  | "transientError" => pure .transientError
  | "authError" => pure .authError
  | "permanentError" => pure .permanentError
  | "cancel" => pure .cancel
  | "retry" => pure .retry
  | other => throw s!"unknown event {other}"

def viewJson : View → Json
  | .connecting => Json.mkObj [("kind", "connecting")]
  | .live => Json.mkObj [("kind", "live")]
  | .reconnecting delay => Json.mkObj [("kind", "reconnecting"), ("delayMs", toJson delay)]
  | .unavailable reason .reopenPage =>
      Json.mkObj [("kind", "unavailable"), ("reason", reasonJson reason)]
  | .closed => Json.mkObj [("kind", "closed")]

/-- Each input is a phase and an event the native watch observed or
enumerated; the expected next phase and its view come from the model. -/
def evaluate (json : Json) : Except String Json := do
  let inputs ← json.getArr?
  let outputs ← inputs.mapM fun input => do
    let phase ← phaseOf (← input.getObjVal? "phase")
    let event ← eventOf (← input.getObjValAs? String "event")
    let next := step phase event
    pure <| Json.mkObj [("next", phaseJson next), ("view", viewJson (view next))]
  pure (Json.arr outputs)

end Conformance.ClientPackWatch

def main : IO Unit := do
  let input ← (← IO.getStdin).getLine
  match Lean.Json.parse input >>= Conformance.ClientPackWatch.evaluate with
  | .ok output => IO.println output.compress
  | .error error => throw <| IO.userError error
