import Lean
import Proofs.ClientShell.PackWatch

namespace Conformance.ClientPackWatch
open Lean ClientShell.PackWatch

def reasonOf : String → Except String TerminalReason
  | "auth" => pure .auth
  | "permanent" => pure .permanent
  | "unreachable" => pure .unreachable
  | other => throw s!"unknown terminal reason {other}"

def reasonJson : TerminalReason → Json
  | .auth => "auth"
  | .permanent => "permanent"
  | .unreachable => "unreachable"

def phaseOf (json : Json) : Except String Phase := do
  match ← json.getObjValAs? String "kind" with
  | "starting" => pure (.starting (← json.getObjValAs? Nat "failures"))
  | "streaming" => pure (.streaming (← json.getObjValAs? Nat "failures"))
  | "backoff" => pure (.backoff (← json.getObjValAs? Nat "failures"))
  | "terminal" => pure (.terminal (← reasonOf (← json.getObjValAs? String "reason")))
  | "cancelled" => pure .cancelled
  | other => throw s!"unknown phase {other}"

def phaseJson : Phase → Json
  | .starting failures => Json.mkObj [("kind", "starting"), ("failures", toJson failures)]
  | .streaming failures => Json.mkObj [("kind", "streaming"), ("failures", toJson failures)]
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

def noticeJson : Option Notice → Json
  | none => Json.null
  | some (.reconnecting delay) => Json.mkObj [("kind", "reconnecting"), ("delayMs", toJson delay)]
  | some (.unavailable reason .reopenPage) =>
      Json.mkObj [("kind", "unavailable"), ("reason", reasonJson reason)]

def viewJson : View → Json
  | .connecting => Json.mkObj [("kind", "connecting")]
  | .live => Json.mkObj [("kind", "live")]
  | .reconnecting delay => Json.mkObj [("kind", "reconnecting"), ("delayMs", toJson delay)]
  | .unavailable reason .reopenPage =>
      Json.mkObj [("kind", "unavailable"), ("reason", reasonJson reason)]
  | .closed => Json.mkObj [("kind", "closed")]

/-- Each input is a phase and an event the native watch observed or
enumerated; the expected next phase, its view and the notice the page is sent
on entering it come from the model. A `budget` input answers the model's
retry budget and delays, which the native constants must equal. -/
def evaluate (json : Json) : Except String Json := do
  let inputs ← json.getArr?
  let outputs ← inputs.mapM fun input => do
    let phase ← phaseOf (← input.getObjVal? "phase")
    let event ← eventOf (← input.getObjValAs? String "event")
    let next := step phase event
    pure <| Json.mkObj [("next", phaseJson next), ("view", viewJson (view next)),
      ("notice", noticeJson (notice next))]
  pure (Json.arr outputs)

def budget : Json :=
  Json.mkObj [("retryBudget", toJson retryBudget),
    ("delayMs", toJson ((List.range (retryBudget + 2)).map delayMs))]

end Conformance.ClientPackWatch

def main : IO Unit := do
  let input ← (← IO.getStdin).getLine
  let evaluated := Lean.Json.parse input >>= fun json =>
    if json == "budget" then pure Conformance.ClientPackWatch.budget
    else Conformance.ClientPackWatch.evaluate json
  match evaluated with
  | .ok output => IO.println output.compress
  | .error error => throw <| IO.userError error
