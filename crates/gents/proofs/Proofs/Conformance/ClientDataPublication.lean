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

private def scopedPublicationCaseJson (name : String)
    (actions : List (String × Option Nat)) : String :=
  let initial : ScopedState := { globalEpoch := 0, nodeEpoch := fun _ => 0 }
  let read := beginScopedRead initial 1
  let current := actions.foldl (fun state action => scopedChange state action.2) initial
  let actionJson (action : String × Option Nat) :=
    "{\"op\":\"" ++ action.1 ++ "\",\"owner\":" ++ optionNatJson action.2 ++ "}"
  "{\"name\":\"" ++ name ++ "\",\"node\":1,\"initial_global_epoch\":0," ++
    "\"initial_node_epochs\":[[1,0],[2,0]],\"actions\":[" ++
    String.intercalate "," (actions.map actionJson) ++ "],\"accepted\":" ++
    toString (acceptsScoped current read) ++ "}"

private def scopedPublicationCasesJson : String :=
  "[" ++ String.intercalate "," [
    scopedPublicationCaseJson "stable_target_accepts_continuous_foreign_events_and_writes"
      ((List.replicate 32 [("request_event", some 2), ("patch", some 2)]).flatten),
    scopedPublicationCaseJson "target_request_event_rejects_scoped_reload"
      [("request_event", some 1)],
    scopedPublicationCaseJson "target_patch_rejects_scoped_reload" [("patch", some 1)],
    scopedPublicationCaseJson "unknown_request_event_rejects_scoped_reload"
      [("request_event", none)],
    scopedPublicationCaseJson "unknown_patch_rejects_scoped_reload" [("patch", none)],
    scopedPublicationCaseJson "global_replacement_rejects_scoped_reload"
      [("replace", none)]
  ] ++ "]"

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
    ",\"scoped_publication\":" ++ scopedPublicationCasesJson ++ "}"

end Conformance.ClientDataPublication

def main : IO Unit := IO.println Conformance.ClientDataPublication.casesJson
