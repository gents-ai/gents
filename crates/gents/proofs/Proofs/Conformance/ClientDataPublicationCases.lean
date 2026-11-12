import Proofs.Clients.ClientShell.ObservationOrdering

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

/-- An action on one node's store while a read of node 1 is in flight. -/
private inductive PublicationAction where
  | merge (node : Nat)
  | request (node : Nat)
  | transcript (node : Nat)

private def applyPublicationAction (state : Fleet × (Nat → String))
    (action : PublicationAction) : Fleet × (Nat → String) :=
  match action with
  | .merge node =>
      (state.1.on node dataChange,
        fun other => if other = node then "concurrent" else state.2 other)
  | .request node => (state.1.on node requestEvent, state.2)
  | .transcript node => (state.1.on node transcriptInvalidation, state.2)

private def publicationActionJson : PublicationAction → String
  | .merge node => "{\"op\":\"merge\",\"node\":" ++ toString node ++ "}"
  | .request node => "{\"op\":\"request\",\"node\":" ++ toString node ++ "}"
  | .transcript node => "{\"op\":\"transcript\",\"node\":" ++ toString node ++ "}"

/-- Two nodes, each with its own store holding one session row. A read of node
1 publishes `incoming` as a patch (`merge`) or a reload after `actions`. -/
private def nodePublicationCaseJson (name : String) (merge : Bool)
    (actions : List PublicationAction) : String := Id.run do
  let initial : State :=
    { dataEpoch := 1, provenanceEpoch := 1, requestEventEpoch := 1, transcriptEpoch := 0 }
  let fleet : Fleet := fun _ => initial
  let read := beginRead (fleet 1)
  let (current, rows) := actions.foldl applyPublicationAction (fleet, fun _ => "initial")
  let accepted := if merge then accepts (current 1) read else acceptsReload (current 1) read
  let expected (node : Nat) : String :=
    if node = 1 ∧ accepted = true then "incoming" else rows node
  return "{\"name\":\"" ++ name ++ "\",\"merge\":" ++ toString merge ++
    ",\"actions\":[" ++ String.intercalate "," (actions.map publicationActionJson) ++
    "],\"accepted\":" ++ toString accepted ++
    ",\"expected\":[{\"node\":1,\"value\":\"" ++ expected 1 ++
    "\"},{\"node\":2,\"value\":\"" ++ expected 2 ++ "\"}]}"

private def nodePublicationCasesJson : String :=
  "[" ++ String.intercalate "," [
    nodePublicationCaseJson "other_node_publication_preserves_reload" false [.merge 2],
    nodePublicationCaseJson "continuous_other_node_publications_preserve_reload" false (List.replicate 64 (.merge 2)),
    nodePublicationCaseJson "same_node_publication_rejects_reload" false [.merge 1],
    nodePublicationCaseJson "same_node_request_event_preserves_reload" false [.request 1],
    nodePublicationCaseJson "sustained_request_events_preserve_reload" false (List.replicate 64 (.request 1)),
    nodePublicationCaseJson "transcript_notice_preserves_reload" false [.transcript 1],
    nodePublicationCaseJson "same_node_request_event_rejects_patch" true [.request 1],
    nodePublicationCaseJson "same_node_publication_rejects_patch" true [.merge 1],
    nodePublicationCaseJson "other_node_request_event_preserves_patch" true [.request 2],
    nodePublicationCaseJson "other_node_publication_preserves_patch" true [.merge 2]
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
  "{\"name\":\"" ++ name ++ "\",\"pending_scopes\":[" ++ String.intercalate "," (recovery.pendingScopes.map toString) ++
  "],\"generation\":" ++ toString recovery.generation ++
  ",\"retry_scheduled\":" ++ toString recovery.retryScheduled ++
  ",\"initial\":{\"pending_scopes\":" ++ natArrayJson initial.pendingScopes ++
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
    { pendingScopes := [], generation := 0, retryScheduled := false }
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
    ",\"node_publication\":" ++ nodePublicationCasesJson ++
    ",\"notice_delivery\":" ++ noticeCasesJson ++ "}"

end Conformance.ClientDataPublication
