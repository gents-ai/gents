import Proofs.ClientShell.ObservationOrdering
import Proofs.Conformance.ClientLiveDelta

open ClientObservationOrdering

private def phaseName : ClientSnapshotObservation.StartupPhase → String
  | .checkingManagedServer => "checking-managed-server"
  | .loadingConfiguration => "loading-configuration"
  | .startingClient => "starting-client"
  | .managedServerError => "managed-server-error"
  | .configurationError => "configuration-error"
  | .clientError => "client-error"
  | .ready => "ready"

private def firstRunName : ClientSnapshotObservation.FirstRunPhase → String
  | .unknown => "unknown"
  | .active => "active"
  | .done => "done"

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
      [false, true].map fun autostartDeclined =>
        "{\"phase\":\"" ++ phaseName phase ++ "\",\"running\":" ++ toString running ++
        ",\"autostartDeclined\":" ++ toString autostartDeclined ++ ",\"expected\":\"" ++
        phaseName (ClientSnapshotObservation.observeStartup phase running autostartDeclined) ++ "\"}"
  let firstRunPhases : List ClientSnapshotObservation.FirstRunPhase := [.unknown, .active, .done]
  let firstRunRows := firstRunPhases.flatMap fun phase =>
    [false, true].flatMap fun snapshotReady =>
      [false, true].flatMap fun initConfigAbsent =>
       [false, true].flatMap fun savedRemote =>
        [false, true].flatMap fun needsSetup =>
          [false, true].map fun authoritative =>
            let expected := ClientSnapshotObservation.observeFirstRun phase snapshotReady initConfigAbsent savedRemote needsSetup authoritative
            "{\"phase\":\"" ++ firstRunName phase ++ "\",\"snapshotReady\":" ++ toString snapshotReady ++
            ",\"initConfigAbsent\":" ++ toString initConfigAbsent ++ ",\"savedRemote\":" ++ toString savedRemote ++ ",\"needsSetup\":" ++ toString needsSetup ++
            ",\"authoritative\":" ++ toString authoritative ++ ",\"expected\":\"" ++ firstRunName expected ++ "\"}"
  IO.println ("{\"fences\":[" ++ String.intercalate "," rows ++
    "],\"startup\":[" ++ String.intercalate "," startupRows ++
    "],\"firstRun\":[" ++ String.intercalate "," firstRunRows ++
    "],\"liveDeltas\":" ++ Conformance.ClientLiveDeltaContracts.casesJson ++ "}")
