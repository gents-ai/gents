import Proofs.ClientShell.ObservationOrdering

open ClientObservationOrdering

private def phaseName : ClientSnapshotObservation.StartupPhase → String
  | .checkingManagedServer => "checking-managed-server"
  | .loadingConfiguration => "loading-configuration"
  | .startingClient => "starting-client"
  | .managedServerError => "managed-server-error"
  | .configurationError => "configuration-error"
  | .clientError => "client-error"
  | .ready => "ready"

/-- Expected guard outcomes are evaluated from the executable Lean owner. -/
def main : IO Unit := do
  let rows := (List.range 5).flatMap fun current =>
    (List.range 5).map fun captured =>
      "{\"current\":" ++ toString current ++
      ",\"captured\":" ++ toString captured ++
      ",\"accepted\":" ++ (if accepts current captured then "true" else "false") ++ "}"
  let phases : List ClientSnapshotObservation.StartupPhase :=
    [.checkingManagedServer, .loadingConfiguration, .startingClient,
     .managedServerError, .configurationError, .clientError, .ready]
  let startupRows := phases.flatMap fun phase =>
    [false, true].flatMap fun running =>
      [false, true].map fun pristine =>
        "{\"phase\":\"" ++ phaseName phase ++ "\",\"running\":" ++ toString running ++
        ",\"pristine\":" ++ toString pristine ++ ",\"expected\":\"" ++
        phaseName (ClientSnapshotObservation.observeStartup phase running pristine) ++ "\"}"
  IO.println ("{\"fences\":[" ++ String.intercalate "," rows ++
    "],\"startup\":[" ++ String.intercalate "," startupRows ++ "]}")
