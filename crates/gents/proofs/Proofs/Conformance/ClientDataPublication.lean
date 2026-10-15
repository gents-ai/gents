import Proofs.ClientShell.ObservationOrdering

namespace Conformance.ClientDataPublication

open _root_.ClientDataPublication

private def stateJson (state : State) : String :=
  "{\"data_epoch\":" ++ toString state.dataEpoch ++
  ",\"provenance_epoch\":" ++ toString state.provenanceEpoch ++
  ",\"request_event_epoch\":" ++ toString state.requestEventEpoch ++
  ",\"transcript_epoch\":" ++ toString state.transcriptEpoch ++ "}"

private def optionNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

private def scopedConfigRowsJson (rows : List ScopedConfigRow) : String :=
  "[" ++ String.intercalate "," (rows.map fun row =>
    "{\"owner\":" ++ toString row.owner ++ ",\"key\":\"" ++ row.key ++
    "\",\"value\":\"" ++ row.value ++ "\",\"source\":" ++
    optionNatJson row.source ++ "}") ++ "]"

private def scopedObservationRowsJson (rows : List ScopedObservationRow) : String :=
  "[" ++ String.intercalate "," (rows.map fun row =>
    "{\"key\":\"" ++ row.key ++ "\",\"value\":\"" ++ row.value ++
    "\",\"source\":" ++ optionNatJson row.source ++ "}") ++ "]"

private def scopedReplacementCaseJson (name : String) (scope : Nat)
    (incomingAuthoritativeOwners incomingTriggerOwners : List Nat)
    (currentConfigs incomingConfigs : List ScopedConfigRow)
    (currentObservations incomingObservations : List ScopedObservationRow) : String := Id.run do
  let expectedConfigs := replaceConfigScope scope currentConfigs incomingConfigs
  let expectedObservations := replaceObservationScope scope currentObservations incomingObservations
  let lookupJson (owner : Nat) : String :=
    let value := (scopedObservationFor owner "shared-trigger" expectedObservations).map
      ScopedObservationRow.value
    "{\"owner\":" ++ toString owner ++ ",\"value\":" ++
      (value.map (fun text => "\"" ++ text ++ "\"")).getD "null" ++ "}"
  return "{\"name\":\"" ++ name ++ "\",\"scope\":" ++
    toString scope ++ ",\"current_configs\":" ++ scopedConfigRowsJson currentConfigs ++
    ",\"incoming_configs\":" ++ scopedConfigRowsJson incomingConfigs ++
    ",\"expected_configs\":" ++ scopedConfigRowsJson expectedConfigs ++
    ",\"current_observations\":" ++ scopedObservationRowsJson currentObservations ++
    ",\"incoming_observations\":" ++ scopedObservationRowsJson incomingObservations ++
    ",\"expected_observations\":" ++ scopedObservationRowsJson expectedObservations ++
    ",\"incoming_authoritative_owners\":[" ++
      String.intercalate "," (incomingAuthoritativeOwners.map toString) ++
    "],\"expected_authoritative_owners\":[" ++
      String.intercalate "," ((incomingAuthoritativeOwners.filter (· == scope)).map toString) ++
    "],\"incoming_trigger_owners\":[" ++
      String.intercalate "," (incomingTriggerOwners.map toString) ++
    "],\"lookups\":[" ++ lookupJson 1 ++ "," ++ lookupJson 2 ++ "," ++ lookupJson 3 ++ "]}"

private def scopedReplacementCasesJson : String :=
  let currentConfigs : List ScopedConfigRow := [
    { owner := 1, key := "shared-session", value := "stale-alpha", source := none },
    { owner := 2, key := "shared-session", value := "beta", source := none }
  ]
  let currentObservations : List ScopedObservationRow := [
    { key := "shared-trigger", value := "unknown", source := none },
    { key := "shared-trigger", value := "stale-alpha", source := some 1 },
    { key := "shared-trigger", value := "beta", source := some 2 }
  ]
  let base := scopedReplacementCaseJson "scoped_replace_preserves_foreign_and_unknown_rows" 1
    [1]
    [1]
    currentConfigs
    [{ owner := 1, key := "shared-session", value := "fresh-alpha", source := some 1 }]
    currentObservations
    [{ key := "shared-trigger", value := "fresh-alpha", source := some 1 }]
  let foreignIncoming := scopedReplacementCaseJson "scoped_replace_ignores_foreign_incoming_rows" 1
    [2]
    []
    currentConfigs
    [{ owner := 2, key := "shared-session", value := "forged-beta", source := some 2 }]
    currentObservations
    [{ key := "shared-trigger", value := "forged-beta", source := some 2 }]
  let duplicateIncoming := scopedReplacementCaseJson "scoped_replace_upserts_duplicate_target_keys" 1
    [1]
    [1]
    currentConfigs
    [ { owner := 1, key := "shared-session", value := "first-alpha", source := some 1 }
    , { owner := 1, key := "shared-session", value := "last-alpha", source := some 1 }
    ]
    currentObservations
    [ { key := "shared-trigger", value := "first-alpha", source := some 1 }
    , { key := "shared-trigger", value := "last-alpha", source := some 1 }
    ]
  "[" ++ String.intercalate "," [base, foreignIncoming, duplicateIncoming] ++ "]"

private def caseJson (name : String) (initial current : State) (read : Read)
    (action : String) : String :=
  "{\"name\":\"" ++ name ++ "\",\"initial\":" ++ stateJson initial ++
  ",\"current\":" ++ stateJson current ++ ",\"read\":{\"data_epoch\":" ++
  toString read.dataEpoch ++ ",\"request_event_epoch\":" ++ toString read.requestEventEpoch ++
  "},\"actions\":[\"" ++ action ++ "\"],\"initial_request_ids\":[\"old\"]," ++
  "\"publish_request_ids\":[\"old\"],\"accepted\":" ++
  toString (accepts current read) ++ "}"

private def natArrayJson (values : List Nat) : String :=
  "[" ++ String.intercalate "," (values.map toString) ++ "]"

private inductive PublicationAction where
  | merge (owners : List Nat)
  | request (owner : Option Nat)
  | unknownObservation
  | replaceAll
  | transcript

private def sessionRow (owner : Nat) (value : String) : ScopedConfigRow :=
  { owner, key := "session-" ++ toString owner, value, source := some owner }

private def applyPublicationAction (state : State × List ScopedConfigRow)
    (action : PublicationAction) : State × List ScopedConfigRow :=
  match action with
  | .merge owners =>
      (dataChange state.1 (.nodes owners),
        upsertConfigRows state.2 (owners.map fun owner => sessionRow owner "concurrent"))
  | .request owner =>
      (requestEventFor state.1 ((owner.map (fun node => Scope.nodes [node])).getD Scope.all),
        state.2)
  | .unknownObservation => (dataChange state.1 .all, state.2)
  | .replaceAll => (dataChange state.1 .all, [sessionRow 2 "replacement"])
  | .transcript => (transcriptInvalidation state.1, state.2)

private def publicationActionJson : PublicationAction → String
  | .merge owners => "{\"op\":\"merge\",\"owners\":" ++ natArrayJson owners ++
      ",\"rows\":" ++ scopedConfigRowsJson (owners.map fun owner => sessionRow owner "concurrent") ++ "}"
  | .request owner => "{\"op\":\"request\",\"owner\":" ++ optionNatJson owner ++ "}"
  | .unknownObservation => "{\"op\":\"unknown_observation\"}"
  | .replaceAll => "{\"op\":\"replace_all\",\"rows\":" ++
      scopedConfigRowsJson [sessionRow 2 "replacement"] ++ "}"
  | .transcript => "{\"op\":\"transcript\"}"

private def scopedPublicationCaseJson (name : String) (owners : Option (List Nat))
    (merge : Bool) (actions : List PublicationAction) : String := Id.run do
  let initial : State :=
    { dataEpoch := 1, provenanceEpoch := 1, requestEventEpoch := 1, transcriptEpoch := 0 }
  let initialRows := [sessionRow 1 "initial", sessionRow 2 "initial"]
  let (current, currentRows) := actions.foldl applyPublicationAction (initial, initialRows)
  let scope := owners.map Scope.nodes |>.getD Scope.all
  let incoming := (owners.getD [1]).map fun owner => sessionRow owner "incoming"
  let accepted := acceptsFor current (beginRead initial) scope
  let expected := if accepted then
      if merge then upsertConfigRows currentRows incoming
      else match owners with
        | some [node] => replaceConfigScope node currentRows incoming
        | _ => incoming
    else currentRows
  let ownersJson := (owners.map natArrayJson).getD "null"
  return "{\"name\":\"" ++ name ++ "\",\"owners\":" ++ ownersJson ++
    ",\"merge\":" ++ toString merge ++ ",\"initial\":" ++ scopedConfigRowsJson initialRows ++
    ",\"incoming\":" ++ scopedConfigRowsJson incoming ++ ",\"actions\":[" ++
    String.intercalate "," (actions.map publicationActionJson) ++
    "],\"accepted\":" ++ toString accepted ++ ",\"expected\":" ++
    scopedConfigRowsJson expected ++ "}"

private def scopedPublicationCasesJson : String :=
  "[" ++ String.intercalate "," [
    scopedPublicationCaseJson "unrelated_node_publication_rebases_scoped_reload" (some [1]) false [.merge [2]],
    scopedPublicationCaseJson "continuous_unrelated_publications_allow_recovery" (some [1]) false (List.replicate 64 (.merge [2])),
    scopedPublicationCaseJson "same_node_publication_rejects_reload" (some [1]) false [.merge [1]],
    scopedPublicationCaseJson "same_node_request_event_rejects_reload" (some [1]) false [.request (some 1)],
    scopedPublicationCaseJson "unrelated_request_event_allows_reload" (some [1]) false [.request (some 2)],
    scopedPublicationCaseJson "unknown_request_event_rejects_reload" (some [1]) false [.request none],
    scopedPublicationCaseJson "unknown_observation_rejects_reload" (some [1]) false [.unknownObservation],
    scopedPublicationCaseJson "full_replacement_rejects_reload" (some [1]) false [.replaceAll],
    scopedPublicationCaseJson "transcript_notice_preserves_reload" (some [1]) false [.transcript],
    scopedPublicationCaseJson "full_read_rejects_unrelated_publication" none false [.merge [2]],
    scopedPublicationCaseJson "patch_uses_all_actual_owners" (some [1, 2]) true [.request (some 2)],
    scopedPublicationCaseJson "patch_rebases_over_unrelated_owner" (some [1]) true [.merge [2]]
  ] ++ "]"

private def requestScopeActions (scopes : List Nat) : List String :=
  scopes.map fun scope => "{\"op\":\"request_scope\",\"scope\":" ++ toString scope ++ "}"

private def recoveryActionsJson (setupScopes failedScopes externalScopes : List Nat)
    (timerAdvanced : Bool) : String :=
  let actions := requestScopeActions setupScopes ++
    ["{\"op\":\"take_batch\"}", "{\"op\":\"fail_batch\",\"scopes\":" ++ natArrayJson failedScopes ++ "}"] ++
    requestScopeActions externalScopes ++
    (if timerAdvanced then ["{\"op\":\"retry_timer\"}"] else [])
  "[" ++ String.intercalate "," actions ++ "]"

private def recoveryJson (name : String) (initial recovery : Recovery)
    (setupScopes failedScopes externalScopes : List Nat) (timerAdvanced : Bool) : String :=
  "{\"name\":\"" ++ name ++ "\",\"full\":" ++ toString recovery.full ++
  ",\"pending_scopes\":[" ++ String.intercalate "," (recovery.pendingScopes.map toString) ++
  "],\"generation\":" ++ toString recovery.generation ++
  ",\"retry_scheduled\":" ++ toString recovery.retryScheduled ++
  ",\"initial\":{\"full\":" ++ toString initial.full ++
  ",\"pending_scopes\":" ++ natArrayJson initial.pendingScopes ++
  ",\"generation\":" ++ toString initial.generation ++
  ",\"retry_scheduled\":" ++ toString initial.retryScheduled ++ "},\"actions\":" ++
  recoveryActionsJson setupScopes failedScopes externalScopes timerAdvanced ++ "}"

private def noticeJson (notice : ClientStoreNotice.Notice) : String :=
  "{\"version\":" ++ toString notice.version ++
  ",\"provenance\":" ++ toString notice.provenance ++
  ",\"request_event\":" ++ toString notice.requestEvent ++ "}"

private def noticeCasesJson : String := Id.run do
  let initial : ClientStoreNotice.Notice := ⟨1, 1, 0⟩
  let deliveries : List ClientStoreNotice.Notice :=
    [⟨3, 2, 1⟩, ⟨2, 1, 1⟩, ⟨3, 2, 1⟩, ⟨5, 3, 2⟩, ⟨4, 2, 2⟩]
  let (_, cases) := deliveries.foldl (fun (current, cases) incoming =>
    let expected := ClientStoreNotice.deliver current incoming
    (expected, cases ++ ["{\"incoming\":" ++ noticeJson incoming ++
      ",\"expected\":" ++ noticeJson expected ++ "}"])) (initial, ([] : List String))
  return "{\"initial\":" ++ noticeJson initial ++ ",\"deliveries\":[" ++
    String.intercalate "," cases ++ "]}"

def casesJson : String := Id.run do
  let initial : State :=
    { dataEpoch := 4, provenanceEpoch := 7, requestEventEpoch := 3, transcriptEpoch := 2 }
  let read := beginRead initial
  let afterUnknownRequest := requestEvent initial
  let afterTranscript := transcriptInvalidation initial
  let afterNewerPublication := publish initial read
  let publicationCases := "[" ++ String.intercalate "," [
    caseJson "uncached_request_event_fences_old_read" initial afterUnknownRequest read "request_event",
    caseJson "transcript_invalidation_preserves_read" initial afterTranscript read "transcript_invalidation",
    caseJson "newer_data_publication_rejects_old_read" initial afterNewerPublication read "replace_requests"
  ] ++ "]"
  let recovery : Recovery :=
    { full := false, pendingScopes := [], generation := 0, retryScheduled := false }
  let setupScopes := [1, 2, 3]
  let queued := setupScopes.foldl requestScope recovery
  let (inFlight, _) := takeBatch queued
  let failed := failBatch inFlight setupScopes
  let externalDuringRetry := requestScope failed 4
  let afterRetryTimer := retryTimer failed
  let recoveryCases := "[" ++ String.intercalate "," [
    recoveryJson "failed_batch_retains_failed_and_unprocessed_scopes" recovery failed setupScopes setupScopes [] false,
    recoveryJson "new_scope_during_retry_is_retained" recovery externalDuringRetry setupScopes setupScopes [4] false,
    recoveryJson "retry_timer_releases_gate_without_losing_scopes" recovery afterRetryTimer setupScopes setupScopes [] true
  ] ++ "]"
  return "{\"publication\":" ++ publicationCases ++ ",\"recovery\":" ++ recoveryCases ++
    ",\"scoped_replacement\":" ++ scopedReplacementCasesJson ++
    ",\"scoped_publication\":" ++ scopedPublicationCasesJson ++
    ",\"notice_delivery\":" ++ noticeCasesJson ++ "}"

end Conformance.ClientDataPublication

def main : IO Unit := IO.println Conformance.ClientDataPublication.casesJson
