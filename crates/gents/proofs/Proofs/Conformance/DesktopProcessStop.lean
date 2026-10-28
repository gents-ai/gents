import Proofs.Background.ProcessControl
import Lean

open Lean Background.ProcessControl

private def decisionName : DesktopStopDecision → String
  | .refuse => "refuse"
  | .alreadyTerminal => "already_terminal"
  | .requestStop => "request_stop"

def main : IO Unit := do
  let owned : Invocation := ⟨"call", "owned-doc"⟩
  let cases := [false, true].flatMap fun authorized =>
    [false, true].flatMap fun background =>
      [false, true].flatMap fun sameDocument =>
        ToolExecution.ToolCallState.all.map fun state =>
          let requested : Invocation := ⟨"call", if sameDocument then "owned-doc" else "other-doc"⟩
          Json.mkObj [
            ("authorized", toJson authorized),
            ("background", toJson background),
            ("sameDocument", toJson sameDocument),
            ("state", toJson state.toDefraDB),
            ("decision", toJson (decisionName (desktopStop authorized background requested owned state)))]
  IO.println (toJson cases).compress
