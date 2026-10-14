import Proofs.ClientShell.ObservationOrdering

namespace Conformance.ClientDataPublication

open _root_.ClientDataPublication

private def stateJson (state : State) : String :=
  "{\"data_epoch\":" ++ toString state.dataEpoch ++
  ",\"provenance_epoch\":" ++ toString state.provenanceEpoch ++
  ",\"request_event_epoch\":" ++ toString state.requestEventEpoch ++
  ",\"transcript_epoch\":" ++ toString state.transcriptEpoch ++ "}"

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
  return "{\"publication\":" ++ publicationCases ++ ",\"recovery\":" ++ recoveryCases ++ "}"

end Conformance.ClientDataPublication

def main : IO Unit := IO.println Conformance.ClientDataPublication.casesJson
