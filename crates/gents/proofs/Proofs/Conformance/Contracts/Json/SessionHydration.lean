import Proofs.SessionHydration
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types

namespace Conformance.Contracts

open Conformance.ContractCases

structure SessionHydrationDecisionCase where
  name : String
  paired : Bool
  pairingRequesterMatches : Bool
  pairingAgentMatches : Bool
  activeMember : Bool
  membershipNetworkMatches : Bool
  ownsSession : Bool

def hydrationRequest : SessionHydration.Request :=
  { key := "peer-1:session-1"
  , peer := "peer-1"
  , requester := "did:key:requester-1"
  , agent := "did:key:agent-1"
  , session := "session-1"
  , nativeSession := 2 }

def hydrationOwnedDocument : SessionHydration.Document :=
  { collection := .agentMessage, id := 1 }

/-- A separately authorized origin dependency is selected exactly even though
its ownership is not re-derived from the target session. -/
def hydrationOriginDependency : SessionHydration.Document :=
  { collection := .agentOutputSegment, id := 2 }

def hydrationClosureInput : SessionHydration.ClosureInput :=
  { request := hydrationRequest
  , bases := []
  , roots := [201]
  , requirements := []
  , messages := [CanonicalOutput.Hydration.Examples.originMessage,
      CanonicalOutput.Hydration.Examples.childMessage]
  , segments := [CanonicalOutput.Hydration.Examples.closing]
  , access := CanonicalOutput.Hydration.Examples.access.map fun observation =>
      { observation with scope := hydrationRequest.authorizationScope }
  , deniedHeaders := []
  , deniedSegments := [] }

def hydrationCatalog (w : SessionHydrationDecisionCase) : SessionHydration.Catalog :=
  { appliedPairingRoutes := if w.paired then
      [{ peer := hydrationRequest.peer
       , requester := if w.pairingRequesterMatches then hydrationRequest.requester else "did:key:requester-2"
       , agent := if w.pairingAgentMatches then hydrationRequest.agent else "did:key:agent-2" }].toFinset
      else ∅
  , selectedNetwork := "network-1"
  , verifiedActiveMemberships := if w.activeMember then
      [{ network := if w.membershipNetworkMatches then "network-1" else "network-2"
       , member := hydrationRequest.requester }].toFinset else ∅
  , sessions := if w.ownsSession then [SessionHydration.ownedSession hydrationRequest].toFinset else ∅
  , closureInputs := [hydrationClosureInput] }

def hydrationTwin : SessionHydration.Request :=
  { hydrationRequest with key := "peer-2:session-1", peer := "peer-2" }

def hydrationCollectionName : CanonicalOutput.Hydration.Collection → String
  | .agentRequest => "AgentRequest"
  | .agentMessage => "AgentMessage"
  | .agentToolCall => "AgentToolCall"
  | .agentOutputSegment => "AgentOutputSegment"
  | .compactionEntry => "CompactionEntry"

def hydrationDocumentKeyJson (key : CanonicalOutput.Hydration.DocumentKey) : String :=
  "{" ++ "\"collection\":" ++ jsonString (hydrationCollectionName key.collection) ++ "," ++
    "\"id\":" ++ toString key.id ++ "}"

def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

def payloadRefJson (reference : CanonicalOutput.PayloadRef) : String :=
  "{" ++ "\"close_id\":" ++ toString reference.closeId ++ "," ++
    "\"stream\":" ++ toString reference.stream ++ "}"

def hydrationMessageJson (message : CanonicalOutput.MessageEnvelope) : String :=
  "{" ++ "\"id\":" ++ toString message.header.id ++ "," ++
    "\"session\":" ++ toString message.header.session ++ "," ++
    "\"request\":" ++ optionalNatJson message.header.request ++ "," ++
    "\"origin\":" ++ optionalNatJson message.header.origin ++ "," ++
    "\"refs\":" ++ jsonArray (message.header.refs.map payloadRefJson) ++ "}"

def sourceJson : CanonicalOutput.Source → String
  | .provider scope turn attempt =>
      "{\"kind\":\"provider\",\"scope\":" ++ toString scope ++
        ",\"turn\":" ++ toString turn ++ ",\"attempt\":" ++ toString attempt ++ "}"
  | .auxiliary kind scope turn attempt =>
      "{\"kind\":\"auxiliary\",\"auxiliary_kind\":" ++
        jsonString (match kind with
          | .compaction => "compaction"
          | .compactionFallback => "compaction_fallback"
          | .title => "title") ++
        ",\"scope\":" ++ toString scope ++
        ",\"turn\":" ++ toString turn ++ ",\"attempt\":" ++ toString attempt ++ "}"
  | .tool call => "{\"kind\":\"tool\",\"owner\":" ++ toString call ++ "}"
  | .authored key => "{\"kind\":\"authored\",\"owner\":" ++ toString key ++ "}"

def writerJson : CanonicalOutput.Writer → String
  | .request generation => "{\"kind\":\"request\",\"owner\":" ++ toString generation ++ "}"
  | .tool call => "{\"kind\":\"tool\",\"owner\":" ++ toString call ++ "}"

def byteListJson (bytes : List UInt8) : String := jsonArray (bytes.map (toString ·.toNat))

def hydrationSegmentJson (segment : CanonicalOutput.Segment) : String :=
  "{" ++ "\"id\":" ++ toString segment.id ++ "," ++
    "\"request\":" ++ toString segment.coordinate.request ++ "," ++
    "\"source\":" ++ sourceJson segment.coordinate.source ++ "," ++
    "\"writer\":" ++ writerJson segment.writer ++ "," ++
    "\"payload\":" ++ byteListJson (segment.flush.map (·.payload) |>.getD []) ++ "}"

def accessStateName : CanonicalOutput.Hydration.AccessState → String
  | .authorized => "authorized" | .missing => "missing" | .denied => "denied"

def hydrationAccessJson (access : CanonicalOutput.Hydration.ProvenanceAccess) : String :=
  "{" ++ "\"key\":" ++ hydrationDocumentKeyJson access.key ++ "," ++
    "\"state\":" ++ jsonString (accessStateName access.state) ++ "," ++
    "\"peer\":" ++ jsonString access.scope.peer ++ "," ++
    "\"requester\":" ++ jsonString access.scope.requester ++ "," ++
    "\"agent\":" ++ jsonString access.scope.agent ++ "," ++
    "\"session\":" ++ jsonString access.scope.session ++ "," ++
    "\"native_session\":" ++ toString access.scope.nativeSession ++ "}"

def hydrationRequestJson (request : SessionHydration.Request) : String :=
  "{" ++ "\"key\":" ++ jsonString request.key ++ "," ++
    "\"peer\":" ++ jsonString request.peer ++ "," ++
    "\"requester\":" ++ jsonString request.requester ++ "," ++
    "\"agent\":" ++ jsonString request.agent ++ "," ++
    "\"session\":" ++ jsonString request.session ++ "," ++
    "\"native_session\":" ++ toString request.nativeSession ++ "}"

def hydrationClosureInputJson (input : SessionHydration.ClosureInput) : String :=
  "{" ++
    "\"request\":" ++ hydrationRequestJson input.request ++ "," ++
    "\"roots\":" ++ jsonArray (input.roots.map toString) ++ "," ++
    "\"messages\":" ++ jsonArray (input.messages.map hydrationMessageJson) ++ "," ++
    "\"segments\":" ++ jsonArray (input.segments.map hydrationSegmentJson) ++ "," ++
    "\"access\":" ++ jsonArray (input.access.map hydrationAccessJson) ++ "," ++
    "\"denied_headers\":" ++ jsonArray (input.deniedHeaders.map toString) ++ "," ++
    "\"denied_segments\":" ++ jsonArray (input.deniedSegments.map toString) ++ "}"

def selectedDocumentsJson (cat : SessionHydration.Catalog) (request : SessionHydration.Request) : String :=
  match SessionHydration.closureInputFor cat.closureInputs request with
  | none => "null"
  | some input =>
      match CanonicalOutput.Hydration.buildManifest input.request.authorizationScope
          input.request.nativeSession input.bases input.roots input.requirements input.messages
          input.segments input.access input.deniedHeaders input.deniedSegments
          input.dependencyDenials with
      | .error _ => "null"
      | .ok manifest => jsonArray (manifest.map hydrationDocumentKeyJson)

def closureDocumentsJson (input : SessionHydration.ClosureInput) : String :=
  match CanonicalOutput.Hydration.buildManifest input.request.authorizationScope
      input.request.nativeSession input.bases input.roots input.requirements input.messages
      input.segments input.access input.deniedHeaders input.deniedSegments input.dependencyDenials with
  | .error _ => "null"
  | .ok manifest => jsonArray (manifest.map hydrationDocumentKeyJson)


def sessionHydrationDecisionCases : List SessionHydrationDecisionCase :=
  [ { name := "admitted", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "unpaired", paired := false,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "pairing_wrong_requester", paired := true,
      pairingRequesterMatches := false, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "pairing_wrong_agent", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := false, activeMember := true,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "inactive_member", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := false,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "foreign_network_member", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := false, ownsSession := true }
  , { name := "unowned_session", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := true, ownsSession := false } ]

/-- Same logical session, but a different peer/request key cannot reuse the
closure admitted for the original hydration request. -/
def hydrationTwinCase : SessionHydrationDecisionCase :=
  { name := "twin", paired := true
  , pairingRequesterMatches := true, pairingAgentMatches := true
  , activeMember := true, membershipNetworkMatches := true, ownsSession := true }

example : SessionHydration.selectedDocuments
    (hydrationCatalog hydrationTwinCase) hydrationTwin = none := by decide

def sessionHydrationDecisionCaseJson (w : SessionHydrationDecisionCase) : String :=
  let cat := hydrationCatalog w
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"paired\":" ++ boolString w.paired ++ ","
    ++ "\"pairing_requester_matches\":" ++ boolString w.pairingRequesterMatches ++ ","
    ++ "\"pairing_agent_matches\":" ++ boolString w.pairingAgentMatches ++ ","
    ++ "\"active_member\":" ++ boolString w.activeMember ++ ","
    ++ "\"membership_network_matches\":" ++ boolString w.membershipNetworkMatches ++ ","
    ++ "\"owns_session\":" ++ boolString w.ownsSession ++ ","
    ++ "\"expected_admit\":" ++ boolString (SessionHydration.decideAdmits cat hydrationRequest) ++ ","
    ++ "\"expected_selected_count\":" ++
      toString ((SessionHydration.selectedDocuments cat hydrationRequest).getD ∅).card ++ ","
    ++ "\"closure_input\":" ++ hydrationClosureInputJson hydrationClosureInput ++ ","
    ++ "\"expected_selected_documents\":" ++ selectedDocumentsJson cat hydrationRequest
    ++ "}"

def sessionHydrationDecisionCasesJson : String :=
  jsonArray (sessionHydrationDecisionCases.map sessionHydrationDecisionCaseJson)

def closureCaseJson (name : String) (input : SessionHydration.ClosureInput)
    (request : SessionHydration.Request := hydrationRequest) : String :=
  let cat := { hydrationCatalog hydrationTwinCase with closureInputs := [input] }
  "{" ++ "\"name\":" ++ jsonString name ++ "," ++
    "\"expected_selected\":" ++ boolString (SessionHydration.selectedDocuments cat request).isSome ++ "," ++
    "\"selection_request\":" ++ hydrationRequestJson request ++ "," ++
    "\"closure_input\":" ++ hydrationClosureInputJson input ++ "," ++
    "\"expected_closure_documents\":" ++ closureDocumentsJson input ++ "," ++
    "\"expected_selected_documents\":" ++ selectedDocumentsJson cat request ++ "}"

def deniedClosureInput (key : CanonicalOutput.Hydration.DocumentKey) :
    SessionHydration.ClosureInput :=
  { hydrationClosureInput with access := hydrationClosureInput.access.map fun observation =>
      if observation.key = key then { observation with state := .denied } else observation }

def sessionHydrationClosureCasesJson : String := jsonArray
  [ closureCaseJson "exact_request_closure" hydrationClosureInput
  , closureCaseJson "hydration_twin_rejected" hydrationClosureInput hydrationTwin
  , closureCaseJson "wrong_root_session_rejected"
      { hydrationClosureInput with request := { hydrationRequest with nativeSession := 1 } }
      { hydrationRequest with nativeSession := 1 }
  , closureCaseJson "message_acp_denied"
      (deniedClosureInput ⟨.agentMessage, 201⟩)
  , closureCaseJson "segment_acp_denied"
      (deniedClosureInput ⟨.agentOutputSegment, 100⟩)
  , closureCaseJson "foreign_scope_evidence_rejected"
      { hydrationClosureInput with access := hydrationClosureInput.access.map fun observation =>
          { observation with scope := { observation.scope with peer := "peer-foreign" } } } ]

structure SessionHydrationApplyCase where
  name : String
  admitted : Bool
  deliveryConfirmed : Bool
  terminalWrite : SessionHydration.TerminalWriteResult

def terminalWriteString : SessionHydration.TerminalWriteResult → String
  | .committed => "committed"
  | .failed => "failed"
  | .notAttempted => "not_attempted"

def sessionHydrationApplyCases : List SessionHydrationApplyCase :=
  [ { name := "admitted_delivery_commits", admitted := true, deliveryConfirmed := true,
      terminalWrite := .committed }
  , { name := "delivered_terminal_write_fails", admitted := true, deliveryConfirmed := true,
      terminalWrite := .failed }
  , { name := "delivered_terminal_write_not_attempted", admitted := true,
      deliveryConfirmed := true, terminalWrite := .notAttempted }
  , { name := "indeterminate_delivery_stays_pending", admitted := true,
      deliveryConfirmed := false, terminalWrite := .notAttempted }
  , { name := "denied_request_rejects", admitted := false, deliveryConfirmed := true,
      terminalWrite := .committed }
  , { name := "denied_terminal_write_fails", admitted := false, deliveryConfirmed := true,
      terminalWrite := .failed } ]

def sessionHydrationApplyCaseJson (w : SessionHydrationApplyCase) : String :=
  let decision : SessionHydrationDecisionCase :=
    { name := w.name
    , paired := w.admitted
    , pairingRequesterMatches := true
    , pairingAgentMatches := true
    , activeMember := true
    , membershipNetworkMatches := true
    , ownsSession := true }
  let cat := hydrationCatalog decision
  let delivery := if w.deliveryConfirmed then SessionHydration.DeliveryResult.confirmed
    else SessionHydration.DeliveryResult.indeterminate
  let initial : SessionHydration.State :=
    { attempted := ∅, confirmedDelivered := ∅, terminals := ∅ }
  let next := SessionHydration.applyStep cat initial hydrationRequest delivery w.terminalWrite
  let selected := (SessionHydration.selectedDocuments cat hydrationRequest).getD ∅
  let served := SessionHydration.terminal hydrationRequest .served selected ∈ next.terminals
  let rejected := SessionHydration.terminal hydrationRequest .rejected ∅ ∈ next.terminals
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"admitted\":" ++ boolString (SessionHydration.decideAdmits cat hydrationRequest) ++ ","
    ++ "\"request\":" ++ hydrationRequestJson hydrationRequest ++ ","
    ++ "\"input_documents\":" ++ selectedDocumentsJson cat hydrationRequest ++ ","
    ++ "\"delivery_confirmed\":" ++ boolString w.deliveryConfirmed ++ ","
    ++ "\"terminal_write\":" ++ jsonString (terminalWriteString w.terminalWrite) ++ ","
    ++ "\"expected_served\":" ++ boolString served ++ ","
    ++ "\"expected_rejected\":" ++ boolString rejected ++ ","
    ++ "\"expected_attempted_count\":" ++ toString next.attempted.card ++ ","
    ++ "\"expected_confirmed_count\":" ++ toString next.confirmedDelivered.card
    ++ "}"

def sessionHydrationApplyCasesJson : String :=
  jsonArray (sessionHydrationApplyCases.map sessionHydrationApplyCaseJson)

structure SessionHydrationProgressCase where
  name : String
  prevSession : String
  prevAgent : String
  session : String
  agent : String
  prevPhase : SessionHydration.ClientPhase
  prevMerged : Nat
  prevServed : Option Nat
  merged : Nat
  served : Option Nat
  servedMatches : Bool := true
  failed : Bool
  beginRequest : Bool

def hydrationDocumentKeys (stem : String) (count : Nat) :
    Finset SessionHydration.DocumentKey :=
  ((List.range count).map fun index =>
    { collection := CanonicalOutput.Hydration.Collection.agentMessage
    , id := if stem = "doc-" then index else index + 1000 }).toFinset

def hydrationManifest (count : Option Nat) (isExact : Bool) :
    Option (Finset SessionHydration.DocumentKey) :=
  count.map fun value => hydrationDocumentKeys (if isExact then "doc-" else "foreign-") value

def progressPrev (w : SessionHydrationProgressCase) : SessionHydration.ClientProgress :=
  { session := w.prevSession
  , agent := w.prevAgent
  , phase := w.prevPhase
  , mergedDocuments := hydrationDocumentKeys "doc-" w.prevMerged
  , servedDocuments := hydrationManifest w.prevServed true }

def progressObserved (w : SessionHydrationProgressCase) : SessionHydration.ClientProgress :=
  SessionHydration.observe (progressPrev w)
    (hydrationDocumentKeys "doc-" w.merged)
    (hydrationManifest w.served w.servedMatches)
    (if w.served.isSome || w.prevServed.isSome then .valid else .loading)
    w.failed w.session w.agent

def sessionHydrationProgressCases : List SessionHydrationProgressCase :=
  [ { name := "open_requests"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .idle, prevMerged := 0, prevServed := none
    , merged := 0, served := none, failed := false, beginRequest := true }
  , { name := "local_documents_do_not_imply_request"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .idle, prevMerged := 0, prevServed := none
    , merged := 2, served := none, failed := false, beginRequest := false }
  , { name := "serving_partial"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .requested, prevMerged := 0, prevServed := none
    , merged := 2, served := some 5, failed := false, beginRequest := false }
  , { name := "complete_when_covered"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .serving, prevMerged := 2, prevServed := some 5
    , merged := 5, served := some 5, failed := false, beginRequest := false }
  , { name := "complete_with_additional_local_documents"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .serving, prevMerged := 2, prevServed := some 5
    , merged := 8, served := some 5, failed := false, beginRequest := false }
  , { name := "empty_session_completes"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .requested, prevMerged := 0, prevServed := none
    , merged := 0, served := some 0, failed := false, beginRequest := false }
  , { name := "cannot_complete_early"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .serving, prevMerged := 2, prevServed := some 5
    , merged := 4, served := some 5, failed := false, beginRequest := false }
  , { name := "equal_count_wrong_documents_cannot_complete"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .serving, prevMerged := 0, prevServed := none
    , merged := 5, served := some 5, servedMatches := false
    , failed := false, beginRequest := false }
  , { name := "failure_is_observed"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .serving, prevMerged := 1, prevServed := some 3
    , merged := 3, served := some 3, failed := true, beginRequest := false }
  , { name := "failed_stays_failed_without_retry"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .failed, prevMerged := 1, prevServed := some 3
    , merged := 3, served := some 3, failed := false, beginRequest := false }
  , { name := "retry_resets_failed"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := .failed, prevMerged := 3, prevServed := some 3
    , merged := 3, served := some 3, failed := false, beginRequest := true }
  , { name := "retry_rejects_other_agent"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-2"
    , prevPhase := .failed, prevMerged := 3, prevServed := some 3
    , merged := 3, served := some 3, failed := false, beginRequest := false }
  , { name := "retry_rejects_other_session"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-2", agent := "agent-1"
    , prevPhase := .failed, prevMerged := 3, prevServed := some 3
    , merged := 0, served := none, failed := false, beginRequest := false }
  , { name := "switch_session_resets_progress"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-2", agent := "agent-1"
    , prevPhase := .complete, prevMerged := 5, prevServed := some 5
    , merged := 0, served := none, failed := false, beginRequest := false }
  ]

def optionNatString : Option Nat → String
  | some n => toString n
  | none => "null"

def phaseString : SessionHydration.ClientPhase → String
  | .idle => "idle"
  | .requested => "requested"
  | .serving => "serving"
  | .complete => "complete"
  | .failed => "failed"

def sessionHydrationProgressCaseJson (w : SessionHydrationProgressCase) : String :=
  let next := if w.beginRequest then SessionHydration.beginRequest w.session w.agent
    else SessionHydration.observe (progressPrev w)
      (hydrationDocumentKeys "doc-" w.merged)
      (hydrationManifest w.served w.servedMatches)
      (if w.served.isSome || w.prevServed.isSome then .valid else .loading)
      w.failed w.session w.agent
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"prev_session\":" ++ jsonString w.prevSession ++ ","
    ++ "\"prev_agent\":" ++ jsonString w.prevAgent ++ ","
    ++ "\"session\":" ++ jsonString w.session ++ ","
    ++ "\"agent\":" ++ jsonString w.agent ++ ","
    ++ "\"prev_phase\":" ++ jsonString (phaseString w.prevPhase) ++ ","
    ++ "\"prev_merged\":" ++ toString w.prevMerged ++ ","
    ++ "\"prev_served\":" ++ optionNatString w.prevServed ++ ","
    ++ "\"merged\":" ++ toString w.merged ++ ","
    ++ "\"served\":" ++ optionNatString w.served ++ ","
    ++ "\"served_matches\":" ++ boolString w.servedMatches ++ ","
    ++ "\"failed\":" ++ boolString w.failed ++ ","
    ++ "\"begin_request\":" ++ boolString w.beginRequest ++ ","
    ++ "\"expected_phase\":" ++ jsonString (phaseString next.phase) ++ ","
    ++ "\"expected_merged\":" ++ toString next.mergedCount ++ ","
    ++ "\"expected_covered\":" ++ toString next.coveredCount ++ ","
    ++ "\"expected_retry_admit\":" ++
      boolString (SessionHydration.canRetry (progressObserved w) w.session w.agent) ++ ","
    ++ "\"expected_complete\":" ++
      boolString (decide (next.phase = SessionHydration.ClientPhase.complete))
    ++ "}"

def sessionHydrationProgressCasesJson : String :=
  jsonArray (sessionHydrationProgressCases.map sessionHydrationProgressCaseJson)

def durableStatusString : SessionHydration.DurableRequest → String
  | .missing => "missing"
  | .pending => "pending"
  | .served _ => "served"
  | .rejected _ => "rejected"

def durableManifest : SessionHydration.DurableRequest →
    Option (Finset SessionHydration.DocumentKey)
  | .missing | .pending => none
  | .served manifest => some manifest
  | .rejected manifest => manifest

def durableServedCount (request : SessionHydration.DurableRequest) : Option Nat :=
  (durableManifest request).map Finset.card

def durableManifestMatches (request : SessionHydration.DurableRequest) : Bool :=
  match durableManifest request with
  | none => true
  | some manifest => manifest = hydrationDocumentKeys "doc-" manifest.card

/-- Reconstruct the modeled request from the exact discriminator and compact
wire fields emitted for the selected conformance cases. -/
def durableRequestRoundtrip (request : SessionHydration.DurableRequest) :
    SessionHydration.DurableRequest :=
  let served := durableServedCount request
  let manifestMatches := durableManifestMatches request
  match request with
  | .missing => .missing
  | .pending => .pending
  | .served _ => .served ((hydrationManifest served manifestMatches).getD ∅)
  | .rejected _ => .rejected (hydrationManifest served manifestMatches)

structure SessionHydrationDurableCase where
  name : String
  request : SessionHydration.DurableRequest
  merged : Nat

def sessionHydrationDurableCases : List SessionHydrationDurableCase :=
  [ { name := "missing_with_local_rows_stays_idle", request := .missing, merged := 3 }
  , { name := "pending_without_rows_is_requested", request := .pending, merged := 0 }
  , { name := "pending_with_rows_is_serving", request := .pending, merged := 2 }
  , { name := "served_waits_for_coverage",
      request := .served (hydrationDocumentKeys "doc-" 5), merged := 2 }
  , { name := "served_completes_at_coverage",
      request := .served (hydrationDocumentKeys "doc-" 5), merged := 5 }
  , { name := "served_completes_with_additional_local_rows",
      request := .served (hydrationDocumentKeys "doc-" 5), merged := 8 }
  , { name := "served_equal_count_wrong_documents_waits",
      request := .served (hydrationDocumentKeys "foreign-" 5), merged := 5 }
  , { name := "empty_served_completes", request := .served ∅, merged := 0 }
  , { name := "rejected_is_failed",
      request := .rejected (some (hydrationDocumentKeys "doc-" 5)), merged := 2 }
  ]

example : sessionHydrationDurableCases.all fun w =>
    durableRequestRoundtrip w.request = w.request := by
  native_decide

def sessionHydrationDurableCaseJson (w : SessionHydrationDurableCase) : String :=
  let next := SessionHydration.projectDurable
    w.request (hydrationDocumentKeys "doc-" w.merged)
      (match w.request with | .served _ => .valid | _ => .loading)
      "session-exact" "agent-exact"
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"status\":" ++ jsonString (durableStatusString w.request) ++ ","
    ++ "\"merged\":" ++ toString w.merged ++ ","
    ++ "\"served\":" ++ optionNatString (durableServedCount w.request) ++ ","
    ++ "\"served_matches\":" ++ boolString (durableManifestMatches w.request) ++ ","
    ++ "\"expected_phase\":" ++ jsonString (phaseString next.phase) ++ ","
    ++ "\"expected_merged\":" ++ toString next.mergedCount ++ ","
    ++ "\"expected_covered\":" ++ toString next.coveredCount
    ++ "}"

def sessionHydrationDurableCasesJson : String :=
  jsonArray (sessionHydrationDurableCases.map sessionHydrationDurableCaseJson)

end Conformance.Contracts
