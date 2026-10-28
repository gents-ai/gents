import Lean
import Proofs.ClientShell.Security

namespace Conformance.DesktopShellSecurity
open Lean ClientShell.Security

private def bools := [false, true]
private def choices : List (Option Bool) := [none, some false, some true]

def cases : Json := Json.mkObj [
  ("navigation", toJson (bools.flatMap fun applicationWindow =>
    ([(false, false, false), (true, false, false), (false, true, false),
      (true, true, false), (false, false, true)] : List (Bool × Bool × Bool)).map fun (application, explorer, sandboxFrame) =>
    Json.mkObj [("applicationWindow", toJson applicationWindow),
      ("application", toJson application), ("explorer", toJson explorer),
      ("sandboxFrame", toJson sandboxFrame),
      ("expected", toJson (navigate applicationWindow application explorer sandboxFrame))])),
  ("plugins", toJson (bools.flatMap fun development => choices.flatMap fun manifest =>
    choices.map fun choice => Json.mkObj [("development", toJson development),
      ("manifest", toJson manifest), ("choice", toJson choice),
      ("expected", toJson (pluginEnabled development manifest choice))])),
  ("send", toJson (bools.flatMap fun gesture => bools.flatMap fun confirmed =>
    bools.map fun sameTarget => Json.mkObj [("gesture", toJson gesture),
      ("confirmed", toJson confirmed), ("sameTarget", toJson sameTarget),
      ("expected", toJson (send gesture confirmed sameTarget))]))]

end Conformance.DesktopShellSecurity

def main : IO Unit := IO.println Conformance.DesktopShellSecurity.cases.compress
