import Proofs.SessionHydration.FileInventoryCases
import Proofs.SessionHydration.FileInventoryOrderCases
import Proofs.SessionHydration.ReaderCases
import Proofs.Conformance.Contracts.Json.SessionHydration
import Proofs.Conformance.Contracts.Json.NativeExecution

namespace Conformance.FileInventoryContracts

open SessionHydration SessionHydration.FileInventory
open SessionHydration.FileInventory.Examples
open Conformance.Contracts
open Conformance.ContractCases

private def object (fields : List (String × String)) : String :=
  "{" ++ String.intercalate "," (fields.map fun (key, value) => jsonString key ++ ":" ++ value) ++ "}"

private def optional {α : Type} (encode : α → String) : Option α → String
  | none => "null"
  | some value => encode value

private def tagged (kind : String) (fields : List (String × String) := []) : String :=
  object (("kind", jsonString kind) :: fields)

private def scopeJson (value : FileImages.Scope) : String := object
  [("requester", jsonString value.requester), ("node", jsonString value.node),
   ("session", jsonString value.session)]

private def failureName : ImageFailure → String
  | .readFailed => "read_failed"
  | .changedDuringRead => "changed_during_read"
  | .tooLarge => "too_large"
  | .unsupportedEntry => "unsupported_entry"
  | .mutationIncomplete => "mutation_incomplete"

private def imageJson : CapturedImage → String
  | .missing => tagged "missing"
  | .file hash data => tagged "file" [("content_hash", jsonString hash), ("data", jsonString data)]
  | .unavailable reason => tagged "unavailable" [("reason", jsonString (failureName reason))]

private def imageMetadataJson : ImageMetadata → String
  | .missing => tagged "missing"
  | .file hash => tagged "file" [("content_hash", jsonString hash)]
  | .unavailable reason => tagged "unavailable" [("reason", jsonString (failureName reason))]

private def capturedChangeJson (value : CapturedChange) : String := object
  [("root", jsonString value.root), ("path", jsonString value.path),
   ("before", imageJson value.before), ("after", imageJson value.after)]

private def changeJson (value : Change) : String := object
  [("root", jsonString value.root), ("path", jsonString value.path),
   ("before", imageMetadataJson value.before), ("after", imageMetadataJson value.after)]

private def observationJson (value : Observation) : String := object
  [("id", optional toString value.id), ("tool", toString value.tool), ("request", toString value.request),
   ("scope", scopeJson value.scope), ("changes", jsonArray (value.changes.map capturedChangeJson)),
   ("verified_unchanged", boolString value.verifiedUnchanged)]

private def projectionJson (value : Projection) : String := object
  [("id", toString value.id), ("observation", optional toString value.observation),
   ("tool", toString value.tool), ("request", toString value.request),
   ("scope", scopeJson value.scope), ("owner", jsonString value.owner),
   ("changes", jsonArray (value.changes.map changeJson)),
   ("verified_unchanged", boolString value.verifiedUnchanged)]

private def toolJson (value : Tool) : String := object
  [("id", toString value.id), ("request", toString value.request), ("scope", scopeJson value.scope),
   ("agent_did", jsonString value.agentDid),
   ("accepted", boolString value.accepted), ("state", jsonString value.state.toDefraDB)]

private def terminalSelectionJson : CanonicalOutput.TerminalSelection → String
  | .noMessage => tagged "no_message"
  | .message id => tagged "message" [("id", toString id)]

/-- Serialization preserves every closure input field. Raw effects remain
unselected inputs to the canonical owner rather than normalized output. -/
private def closureJson (value : ClosureInput) : String := object
  [("request", hydrationRequestJson value.request),
   ("bases", jsonArray (value.bases.map (hydrationDocumentKeyJson ·.key))),
   ("roots", jsonArray (value.roots.map toString)),
   ("requirements", jsonArray (value.requirements.map fun row => object
      [("request", toString row.request), ("session", toString row.session),
       ("selection", optional terminalSelectionJson row.selection)])),
   ("messages", jsonArray (value.messages.map canonicalMessageJson)),
   ("segments", jsonArray (value.segments.map canonicalSegmentJson)),
   ("access", jsonArray (value.access.map hydrationAccessJson)),
   ("denied_headers", jsonArray (value.deniedHeaders.map toString)),
   ("denied_segments", jsonArray (value.deniedSegments.map toString)),
   ("dependency_denials", jsonArray (value.dependencyDenials.map fun row => object
      [("root_close_id", toString row.rootCloseId), ("denied_doc_id", toString row.deniedDocId)])),
   ("effects", jsonArray (value.effects.map hydrationEffectJson))]

private def sessionOwnerJson (row : SessionOwner) : String := object
  [("session", jsonString row.session), ("native_session", toString row.nativeSession),
   ("requester", jsonString row.requester), ("node", jsonString row.node)]

private def sessionReadJson (value : SessionReadObservation) : String := object
  [("scope", object [("peer", jsonString value.scope.peer),
      ("requester", jsonString value.scope.requester), ("node", jsonString value.scope.node),
      ("session", jsonString value.scope.session),
      ("native_session", toString value.scope.nativeSession)]),
   ("source", sessionOwnerJson value.source), ("visible", boolString value.visible)]

private def catalogJson (value : CatalogInput) : String := object
  [("applied_pairing_routes", jsonArray (value.appliedPairingRoutes.map fun row => object
      [("peer", jsonString row.peer), ("requester", jsonString row.requester), ("node", jsonString row.node)])),
   ("selected_network", jsonString value.selectedNetwork),
   ("verified_active_memberships", jsonArray (value.verifiedActiveMemberships.map fun row => object
      [("network", jsonString row.network), ("member", jsonString row.member)])),
   ("sessions", jsonArray (value.sessions.map sessionOwnerJson)),
   ("session_reads", jsonArray (value.sessionReads.map sessionReadJson)),
   ("closure_inputs", jsonArray (value.closureInputs.map closureJson))]

private def boundaryName : ReadBoundary → String
  | .atomicExhaustive => "atomic_exhaustive"
  | .readerBound _ => "reader_bound"
  | .incomplete => "incomplete"
  | .nonAtomic => "non_atomic"

private def sourceJson (value : SourceInput) : String := object
  [("catalog", catalogJson value.catalog), ("tools", jsonArray (value.tools.map toolJson)),
   ("projections", jsonArray (value.projections.map projectionJson)),
   ("read_scope", match value.boundary with
    | .readerBound scope => object [("peer", jsonString scope.peer),
        ("requester", jsonString scope.requester), ("node", jsonString scope.node),
        ("session", jsonString scope.session), ("native_session", toString scope.nativeSession)]
    | _ => "null"),
   ("boundary", jsonString (boundaryName value.boundary))]

private def captureJson : Capture → String
  | .pending => tagged "pending"
  | .unknown => tagged "unknown"
  | .observed projection => tagged "observed" [("projection", projectionJson projection)]

private def windowJson (value : Order.Window) : String := object
  [("tool", toString value.tool), ("admitted", toString value.admitted),
   ("delivered", toString value.delivered)]

private def snapshotJson (value : Snapshot) : String := object
  [("admission", hydrationRequestJson value.admission),
   ("documents", jsonArray (value.documents.map hydrationDocumentKeyJson)),
   ("tools", jsonArray (value.tools.map fun row => object
      [("tool", toolJson row.tool), ("capture", captureJson row.capture)])),
   ("ordering", jsonArray (value.ordering.map (optional windowJson)))]

private def selectionResultJson (result : Except Error Snapshot) : String :=
  match result with
  | .error .rejected => tagged "rejected"
  | .error .unknown => tagged "unknown"
  | .ok value => tagged "selected" [("snapshot", snapshotJson value),
      ("known_empty", boolString value.knownEmpty)]

private def selectionJson (source : SourceInput) (request : Request) : String :=
  selectionResultJson (snapshot source.model request)

private def selectionCaseJson (value : SelectionCase) : String := object
  [("name", jsonString value.name), ("source", sourceJson value.source),
   ("admission", hydrationRequestJson admission), ("expected", selectionJson value.source admission)]

def forkCasesJson : String := jsonArray [object
  [("name", jsonString "authorized_fork_origin"), ("source", sourceJson forkSource),
   ("admission", hydrationRequestJson forkRequest), ("expected", selectionJson forkSource forkRequest)]]

private def actionJson : ToolExecution.ToolCallContext.Action → String
  | .dispatch => tagged "dispatch"
  | .spawnFailed failure => tagged "spawn_failed" [("failure", jsonString failure.toDefraDB)]
  | .complete => tagged "complete"
  | .fail failure => tagged "fail" [("failure", jsonString failure.toDefraDB)]
  | .timeout => tagged "timeout"
  | .background => tagged "background"
  | .foreground => tagged "foreground"
  | .cancelBeforeDispatch cause => tagged "cancel_before_dispatch" [("cause", jsonString cause.toDefraDB)]
  | .cancelDuringRun cause => tagged "cancel_during_run" [("cause", jsonString cause.toDefraDB)]

private def publicationCaseJson (value : PublicationCase) : String := object
  [("name", jsonString value.name),
   ("pre", "{" ++ Conformance.NativeExecutionContracts.contextFieldsJson value.pre ++ "}"),
   ("action", actionJson value.action), ("won", boolString value.won),
   ("write", jsonString (terminalWriteString value.write)), ("actual", toolJson value.actual),
   ("observation", observationJson value.observation), ("projection_id", "3001"),
   ("expected", optional projectionJson
      (publish value.pre value.action value.won value.write value.actual value.observation 3001))]

private def coverageJson (value : Coverage) : String := object
  [("entries", toString value.entries), ("pending", toString value.pending),
   ("unknown", toString value.unknown)]

private def entryJson (value : Entry) : String := object
  [("tool", toString value.tool), ("request", toString value.request), ("scope", scopeJson value.scope),
   ("projection", toString value.projection), ("observation", toString value.observation),
   ("change", changeJson value.change),
   ("request_coverage", optional coverageJson value.requestCoverage),
   ("order", match value.order with
     | .unresolved => tagged "unresolved"
     | .ordered index entries => tagged "ordered" [("index", toString index), ("entries", toString entries)])]

private def cursorJson (value : Cursor) : String := object
  [("snapshot", snapshotJson value.snapshot), ("offset", toString value.offset)]

private def pageJson (value : Page) : String := object
  [("snapshot", snapshotJson value.snapshot), ("offset", toString value.offset),
   ("entries", jsonArray (value.entries.map entryJson)), ("next", optional cursorJson value.next)]

private def refusalReasonJson : RefusalReason → String
  | .rejected => jsonString "rejected"
  | .stale => jsonString "stale"
  | .unavailable => jsonString "unavailable"


private def sourceProjectionCaseJson (value : SelectionCase) : String :=
  let pages := match snapshot value.source.model admission with
    | .error _ => []
    | .ok current => (List.range (max 1 ((current.entries.length + pageLimit - 1) / pageLimit))).map
        (fun index => pageJson (makePage current (index * pageLimit)))
  object [("name", jsonString value.name), ("source", sourceJson value.source),
    ("admission", hydrationRequestJson admission), ("expected", selectionJson value.source admission),
    ("refusal", match snapshot value.source.model admission with
      | .error error => refusalReasonJson error.refusal
      | .ok _ => "null"),
    ("pages", jsonArray pages)]

def readerCasesJson : String := jsonArray ((SessionHydration.ReaderExamples.cases.filter fun value =>
  ["shared_native_root_read", "shared_authorized_empty_root",
   "shared_root_physical_identity_changed", "shared_missing_capture_is_not_empty_complete",
   "shared_unreadable_projection_owner_is_not_trusted"].contains value.name).map fun value =>
  object [("name", jsonString value.name), ("source", sourceJson value.source),
    ("expected", selectionJson value.source admission),
    ("coverage", match snapshot value.source.model admission with
      | .error _ => "null"
      | .ok current => coverageJson current.coverage)])

/-- Native projection checks refine this subset of the executable source cases.
Canonical admission, fork selection and transport remain separate consumers. -/
def sourceProjectionCasesJson : String :=
  let selected := selectionCases.filter fun value =>
    ["verified_unchanged_terminal", "unchanged_claim_cannot_carry_images",
     "unchanged_claim_cannot_retain_image_identity", "unchanged_claim_cannot_precede_terminal",
     "accepted_terminal_capture", "legacy_missing_capture_is_unknown",
     "active_missing_capture_is_pending", "uncommitted_capture_before_terminal",
     "projection_wrong_request", "projection_in_another_session_is_not_this_inventory",
     "projection_wrong_actual_requester", "projection_wrong_actual_node",
     "projection_foreign_stored_owner", "invalid_empty_capture_not_empty_inventory",
     "duplicate_paths_in_capture"].contains value.name
  let largeCase : SelectionCase := ⟨"one_capture_exceeds_one_page", oneLargeCapture, .selected 65 0 0 false⟩
  jsonArray ((selected ++ [largeCase]).map sourceProjectionCaseJson)

private def bindingJson (value : PageBinding) : String := object
  [("admission", hydrationRequestJson value.admission), ("digest", jsonString value.digest),
   ("coverage", coverageJson value.coverage)]

private def pageCursorJson (value : PageCursor) : String := object
  [("binding", bindingJson value.binding), ("offset", toString value.offset)]

private def wirePageJson (value : WirePage) : String := object
  [("binding", bindingJson value.binding), ("offset", toString value.offset),
   ("entries", jsonArray (value.entries.map entryJson)), ("next", optional pageCursorJson value.next)]

private def envelopeJson (value : PageEnvelope) : String := object
  [("purpose", jsonString (match value.purpose with
      | .transcriptV1 => "transcript_v1"
      | .fileInventoryV2 => "file_inventory_v2")),
   ("request_key", jsonString value.requestKey), ("signer", jsonString value.signer),
   ("signature_verified", boolString value.signatureVerified), ("page", wirePageJson value.page)]

private def demandJson (value : PageDemand) : String := object
  [("admission", hydrationRequestJson value.admission), ("attempt", jsonString value.attempt),
   ("cursor", optional pageCursorJson value.cursor)]

def selectedImageCasesJson : String := jsonArray (SessionHydration.ReaderExamples.imageCases.map fun value =>
  object [("name", jsonString value.name), ("catalog", catalogJson value.catalog),
    ("admission", hydrationRequestJson value.demand.image.admission),
    ("request_doc_id", jsonString value.demand.image.requestDocId),
    ("tool_doc_id", jsonString value.demand.image.toolDocId),
    ("metadata", demandJson value.demand.metadata),
    ("digest", jsonString value.demand.digest), ("entry_index", toString value.demand.entryIndex),
    ("page", envelopeJson value.page), ("nonce_matches", boolString value.nonceMatches),
    ("page_owner", jsonString value.pageOwner),
    ("read", object [("scope", object [("peer", jsonString value.read.scope.peer),
        ("requester", jsonString value.read.scope.requester), ("node", jsonString value.read.scope.node),
        ("session", jsonString value.read.scope.session), ("native_session", toString value.read.scope.nativeSession)]),
      ("projection", toString value.read.projection), ("observation", toString value.read.observation),
      ("projection_visible", boolString value.read.projectionVisible),
      ("image_visible", boolString value.read.imageVisible),
      ("projection_owner", jsonString value.read.projectionOwner),
      ("image_owner", jsonString value.read.imageOwner)]),
    ("tools", jsonArray (value.tools.map fun tool => object
      [("doc_id", jsonString tool.docId), ("request_doc_id", jsonString tool.requestDocId),
       ("scope", scopeJson tool.scope), ("accepted", boolString tool.accepted), ("terminal", boolString tool.terminal)])),
    ("images", jsonArray (value.images.map fun image => object
      [("doc_id", jsonString image.docId), ("tool_call_doc_id", jsonString image.toolDocId),
       ("request_doc_id", jsonString image.requestDocId), ("scope", scopeJson image.scope),
       ("contents", jsonArray (image.contents.map fun content => object
         [("doc_id", jsonString content.docId), ("tool_call_doc_id", jsonString content.toolDocId), ("scope", scopeJson content.scope)]))])),
    ("delivery_documents", jsonArray (match value.result with
      | .selected document => match value.images.find? (fun image => image.docId == document) with
        | some image => (FileImages.deliveryDocuments image).map jsonString
        | none => []
      | _ => [])),
    ("expected", match value.result with
      | .rejected => tagged "rejected"
      | .missing => tagged "missing"
      | .selected document => tagged "selected" [("document", jsonString document)])])
private def directSource (visible : Bool) : SourceInput :=
  let current := Examples.source 1
  let root := { ownedSession admission with requester := "session-creator" }
  { current with
    boundary := .readerBound admission.authorizationScope
    catalog := { current.catalog with
      appliedPairingRoutes := []
      verifiedActiveMemberships := []
      sessions := [root]
      sessionReads := [⟨admission.authorizationScope, root, visible⟩] } }

private def directCases : List (String × String × SourceInput) :=
  [("native_reader_of_agent_owned_projection", admission.requester, directSource true),
   ("anonymous", "", directSource true),
   ("actor_differs_from_bound_reader", "other-reader", directSource true),
   ("native_session_denial", admission.requester, directSource false),
   ("privileged_source_is_not_reader_evidence", admission.requester,
     {directSource true with boundary := .atomicExhaustive}),
   ("wrong_projection_agent_owner", admission.requester,
     {directSource true with projections := [{projection 1 with owner := admission.node}]}),
   ("missing_capture_remains_unknown", admission.requester,
     {directSource true with projections := []})]

theorem direct_reader_cases_pinned :
    directCases.map (fun (_, actor, source) =>
      match directSnapshot source.model actor admission with
      | .ok _ => true
      | .error _ => false) =
    [true, false, false, false, false, false, true] := by native_decide

def directReaderCasesJson : String :=
  jsonArray (directCases.map fun (name, actor, source) =>
    let result := directSnapshot source.model actor admission
    object [("name", jsonString name), ("actor", jsonString actor),
      ("admission", hydrationRequestJson admission), ("source", sourceJson source),
      ("expected", selectionResultJson result),
      ("page", match result with
        | .error _ => "null"
        | .ok current => wirePageJson ((makePage current 0).toWire "direct-snapshot")),
      ("wrong_physical_expected", selectionResultJson (directSnapshot source.model actor
          {admission with nativeSession := admission.nativeSession + 1}))])

def orderingWindowCasesJson : String := jsonArray (Order.Examples.windowCases.map
  fun (name, selected, closure, tool) => object
    [("name", jsonString name), ("snapshot", snapshotJson selected),
     ("closure", closureJson closure), ("tool", toolJson tool),
     ("expected", optional windowJson (Order.window selected closure tool))])

def orderingResolutionCasesJson : String := jsonArray (Order.Examples.resolutionCases.map
  fun (name, windows) => object
    [("name", jsonString name), ("windows", jsonArray (windows.map windowJson)),
     ("expected", optional (fun ids => jsonArray (ids.map toString)) (Order.resolve windows))])

def orderingComparisonCasesJson : String := jsonArray (Order.Examples.comparisonCases.map
  fun (name, selected, closure, node, root, path) => object
    [("name", jsonString name), ("snapshot", snapshotJson selected),
     ("closure", closureJson closure), ("node", jsonString node),
     ("root", jsonString root), ("path", jsonString path),
     ("expected", optional (fun ids => jsonArray (ids.map toString))
       (Order.comparison selected closure node root path)),
     ("request_coverage", jsonArray (selected.tools.map fun fact => object
       [("tool", toString fact.tool.id),
        ("coverage", coverageJson (selected.requestCoverage fact.tool.scope fact.tool.request))]))])

def orderingEntryCasesJson : String := jsonArray (Order.Examples.entryCases.map
  fun (name, entries) => object
    [("name", jsonString name), ("entries", jsonArray (entries.map entryJson)),
     ("expected", boolString (coherentEntries entries))])

/-- IDs, DIDs and image hashes are the existing models' abstract names. Native
adapters bind them to actual Defra-created physical IDs, authenticated key
identities and validated captures through their existing decode owners. Empty
or malformed native IDs cannot become modeled observations; abstract hash
labels cannot bypass the capture decoder's byte/hash checks. These are
executable inputs/outputs, not a production wire-format proposal. -/
def publicationCasesJson : String := jsonArray (publicationCases.map publicationCaseJson)

end Conformance.FileInventoryContracts
