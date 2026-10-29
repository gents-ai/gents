import Proofs.Session.ExecutionActivity
import Lean

open Lean Session.ExecutionActivity

def main : IO Unit := do
  let identity : Identity := ⟨"request", "generation"⟩
  let observation : Observation := ⟨identity, 0, 0, 0, 10, .providerRetry⟩
  let cases := [false, true].flatMap fun terminal =>
    [false, true].flatMap fun pending =>
      [false, true].map fun hasGeneration =>
        let display := projectRequest terminal pending (if hasGeneration then some identity else none) 10 1 (some observation)
        let name := match display with
          | .terminal => "terminal"
          | .queued => "queued"
          | .retrying => "retrying"
          | _ => "unknown"
        Json.mkObj [("terminal", toJson terminal), ("pending", toJson pending),
          ("hasGeneration", toJson hasGeneration), ("display", toJson name)]
  IO.println (toJson cases).compress
