import Proofs.Clients.DesktopConnect

open DesktopConnect

private def nodeName : NodeState → String
  | .notRunning => "not_running"
  | .olderHome => "older_home"
  | .outdated => "outdated"
  | .serving => "serving"

private def screenName : Screen → String
  | .loadingSettings => "loading_settings"
  | .notConnected => "not_connected"

private def refusalName : Option Refusal → String
  | none => "null"
  | some .nodeNotRunning => "\"node_not_running\""
  | some .olderHome => "\"older_home\""
  | some .outdatedNode => "\"outdated_node\""
  | some .enrollmentRefused => "\"enrollment_refused\""
  | some .approvalPending => "\"approval_pending\""

def main : IO Unit := do
  let rows := [NodeState.notRunning, .olderHome, .outdated, .serving].flatMap fun node =>
    [false, true].map fun enrolled =>
      let (shown, refusal) := screen node enrolled
      "{\"node\":\"" ++ nodeName node ++ "\",\"enrolled\":" ++ toString enrolled ++
      ",\"screen\":\"" ++ screenName shown ++ "\",\"refusal\":" ++ refusalName refusal ++ "}"
  IO.println ("[" ++ String.intercalate "," rows ++ "]")
