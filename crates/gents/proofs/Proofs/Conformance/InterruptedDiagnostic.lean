import Proofs.Session.ToolExecution.InterruptedDiagnostic
import Proofs.Conformance.Family

namespace Conformance.InterruptedDiagnostic

open Lean ToolExecution ToolExecution.InterruptedDiagnostic

/-- A running call with `raw` output; the Agent's bash budget is
`configuredBudget` when the call times out (unset Tools when `none`), the call
is reloaded before it times out when `reload`, and a stale handle replays the
deadline after the budget is reconfigured to `laterBudget`. -/
structure Interruption where
  toolName : String
  raw : String
  configuredBudget : Option Nat
  reload : Bool
  laterBudget : Nat
  deriving ToJson

structure Diagnostic where
  timedOut : Bool
  tail : String
  replayTimedOut : Bool
  deriving ToJson

private def interruption (toolName : String) (configuredBudget : Option Nat) (reload : Bool) :
    Interruption :=
  { toolName, raw := "0123456789abcdefghij", configuredBudget, reload, laterBudget := 12 }

@[conformance_family]
def budget : Conformance.Family Interruption Diagnostic where
  inputs :=
    [ interruption "bash" (some 5) false
    , interruption "bash" (some 5) true
    , interruption "mcp__search__query" (some 5) false
    , interruption "bash" none false
    , interruption "bash" none true
    , interruption "mcp__search__query" none false ]
  model := fun i =>
    let shown := tail i.raw.toUTF8.toList (outputBudget i.toolName i.configuredBudget)
    { timedOut := deadlineWins .running
    , tail := (String.fromUTF8? (ByteArray.mk shown.toArray)).getD ""
    , replayTimedOut := deadlineWins (afterDeadline .running) }

end Conformance.InterruptedDiagnostic
