import Proofs.Session.ExecutionActivity
import Lean

open Lean Session.ExecutionActivity

def main : IO Unit := do
  let cases := [false, true].flatMap fun retry =>
    [false, true].map fun unreachable =>
      let phase := match failurePhase retry unreachable with
        | .providerRetry => "provider_retry"
        | .unavailable => "unavailable"
        | _ => "unknown"
      Json.mkObj [("retry", toJson retry), ("unreachable", toJson unreachable), ("phase", toJson phase)]
  IO.println (toJson cases).compress
