import Proofs.Session.ExecutionActivity
import Lean

open Lean Session.ExecutionActivity

private def phaseName : Phase → String
  | .queued => "queued"
  | .generating => "generating"
  | .toolWait => "tool_wait"
  | .providerRetry => "provider_retry"
  | .unavailable => "unavailable"
  | .terminal => "terminal"

private def displayName : Display → String
  | .unknown => "unknown"
  | .queued => "queued"
  | .working => "working"
  | .waiting => "waiting"
  | .retrying => "retrying"
  | .unavailable => "unavailable"
  | .terminal => "terminal"

def main : IO Unit := do
  let active : Identity := ⟨"request", "generation"⟩
  let phases : List Phase := [.queued, .generating, .toolWait, .providerRetry, .unavailable, .terminal]
  let cases := [false, true].flatMap fun terminal =>
    [false, true].flatMap fun sameGeneration =>
      [8, 10, 11, 15].flatMap fun observedAt =>
        phases.map fun phase =>
          let identity : Identity := ⟨"request", if sameGeneration then "generation" else "other"⟩
          let observation : Observation := ⟨identity, 2, 0, 1, observedAt, phase⟩
          Json.mkObj [
            ("terminal", toJson terminal), ("sameGeneration", toJson sameGeneration),
            ("observedAt", toJson observedAt), ("now", toJson (10 : Nat)),
            ("maxAge", toJson (1 : Nat)), ("phase", toJson (phaseName phase)),
            ("display", toJson (displayName (project active terminal 10 1 (some observation))))]
  IO.println (toJson cases).compress
