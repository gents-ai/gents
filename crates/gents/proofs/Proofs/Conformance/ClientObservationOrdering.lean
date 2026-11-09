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

private def startupFailureName : ClientSnapshotObservation.StartupFailure → String
  | .approval => "approval"
  | .portOccupied => "port-occupied"
  | .foreignNode => "foreign-node"
  | .homeNeedsFreshStart => "home-needs-fresh-start"
  | .runtimeFailed => "runtime-failed"
  | .outdated => "outdated"
  | .transient => "transient"

private def startupActionName : ClientSnapshotObservation.StartupAction → String
  | .openLoginItems => "open-login-items"
  | .checkAgain => "check-again"
  | .backUpAndStartFresh => "back-up-and-start-fresh"
  | .removeAndStartFresh => "remove-and-start-fresh"
  | .restartNode => "restart-node"
  | .showDiagnostics => "show-diagnostics"
  | .updateNode => "update-node"
  | .tryAgain => "try-again"
  | .continueWithoutLocal => "continue-without-local"

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
  let initStatuses : List (ClientSnapshotObservation.InitConfigStatus × String) :=
    [(.absent, "absent"), (.configured, "configured"), (.unreadable, "unreadable"), (.unknown, "unknown")]
  let firstRunRows := firstRunPhases.flatMap fun phase =>
    [false, true].flatMap fun snapshotReady =>
      initStatuses.flatMap fun (initStatus, initName) =>
        [false, true].flatMap fun needsSetup =>
          [false, true].flatMap fun authoritative =>
            [false, true].flatMap fun localRuntimeAllowed =>
              [false, true].flatMap fun localAvailable =>
                [false, true].map fun remoteConfigured =>
                  let expected := ClientSnapshotObservation.observeFirstRun phase snapshotReady initStatus needsSetup authoritative localRuntimeAllowed localAvailable remoteConfigured
                  "{\"phase\":\"" ++ firstRunName phase ++ "\",\"snapshotReady\":" ++ toString snapshotReady ++
                  ",\"initConfigStatus\":\"" ++ initName ++ "\",\"needsSetup\":" ++ toString needsSetup ++
                  ",\"authoritative\":" ++ toString authoritative ++ ",\"localRuntimeAllowed\":" ++ toString localRuntimeAllowed ++
                  ",\"localAvailable\":" ++ toString localAvailable ++
                  ",\"remoteConfigured\":" ++ toString remoteConfigured ++ ",\"expected\":\"" ++ firstRunName expected ++ "\"}"
  let startupFailureRows := ClientSnapshotObservation.allStartupFailures.map fun failure =>
    "{\"failure\":\"" ++ startupFailureName failure ++ "\",\"actions\":[" ++
      String.intercalate "," (failure.actions.map fun action =>
        "\"" ++ startupActionName action ++ "\"") ++ "]}"
  IO.println ("{\"fences\":[" ++ String.intercalate "," rows ++
    "],\"startup\":[" ++ String.intercalate "," startupRows ++
    "],\"firstRun\":[" ++ String.intercalate "," firstRunRows ++
    "],\"startupFailures\":[" ++ String.intercalate "," startupFailureRows ++
    "],\"liveDeltas\":" ++ Conformance.ClientLiveDeltaContracts.casesJson ++ "}")
