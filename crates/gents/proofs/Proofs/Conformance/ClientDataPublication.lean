import Proofs.ClientShell.ObservationOrdering

namespace Conformance.ClientDataPublication

open _root_.ClientDataPublication

private def stateJson (state : State) : String :=
  "{\"data_epoch\":" ++ toString state.dataEpoch ++
  ",\"provenance_epoch\":" ++ toString state.provenanceEpoch ++
  ",\"request_event_epoch\":" ++ toString state.requestEventEpoch ++
  ",\"transcript_epoch\":" ++ toString state.transcriptEpoch ++ "}"

private def caseJson (name : String) (initial current : State) (read : Read) : String :=
  "{\"name\":\"" ++ name ++ "\",\"initial\":" ++ stateJson initial ++
  ",\"current\":" ++ stateJson current ++ ",\"read\":{\"data_epoch\":" ++
  toString read.dataEpoch ++ ",\"request_event_epoch\":" ++ toString read.requestEventEpoch ++
  "},\"accepted\":" ++ toString (accepts current read) ++ "}"

private def recoveryJson (name : String) (recovery : Recovery) : String :=
  "{\"name\":\"" ++ name ++ "\",\"full\":" ++ toString recovery.full ++
  ",\"pending_scopes\":[" ++ String.intercalate "," (recovery.pendingScopes.map toString) ++
  "],\"generation\":" ++ toString recovery.generation ++
  ",\"retry_scheduled\":" ++ toString recovery.retryScheduled ++ "}"

def casesJson : String := Id.run do
  let initial : State :=
    { dataEpoch := 4, provenanceEpoch := 7, requestEventEpoch := 3, transcriptEpoch := 2 }
  let read := beginRead initial
  let afterUnknownRequest := requestEvent initial
  let afterTranscript := transcriptInvalidation initial
  let afterNewerPublication := publish initial read
  let publicationCases := "[" ++ String.intercalate "," [
    caseJson "uncached_request_event_fences_old_read" initial afterUnknownRequest read,
    caseJson "transcript_invalidation_preserves_read" initial afterTranscript read,
    caseJson "newer_data_publication_rejects_old_read" initial afterNewerPublication read
  ] ++ "]"
  let recovery : Recovery :=
    { full := false, pendingScopes := [], generation := 5, retryScheduled := false }
  let failed := failBatch recovery [1, 2, 3]
  let externalDuringRetry := requestScope failed 4
  let afterRetryTimer := retryTimer failed
  let recoveryCases := "[" ++ String.intercalate "," [
    recoveryJson "failed_batch_retains_failed_and_unprocessed_scopes" failed,
    recoveryJson "new_scope_during_retry_is_retained" externalDuringRetry,
    recoveryJson "retry_timer_releases_gate_without_losing_scopes" afterRetryTimer
  ] ++ "]"
  return "{\"publication\":" ++ publicationCases ++ ",\"recovery\":" ++ recoveryCases ++ "}"

end Conformance.ClientDataPublication

def main : IO Unit := IO.println Conformance.ClientDataPublication.casesJson
