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
     "projection_wrong_request", "projection_wrong_actual_session",
     "projection_wrong_actual_requester", "projection_wrong_actual_node",
     "projection_foreign_stored_owner", "invalid_empty_capture_not_empty_inventory",
     "duplicate_paths_in_capture"].contains value.name
  let largeCase : SelectionCase := ⟨"one_capture_exceeds_one_page", oneLargeCapture, .selected 65 0 0 false⟩
  jsonArray ((selected ++ [largeCase]).map sourceProjectionCaseJson)

private def receiptJson : VerifiedReceipt → String
  | .transcriptV1 request documents => tagged "transcript_v1"
      [("admission", hydrationRequestJson request),
       ("documents", jsonArray (documents.map hydrationDocumentKeyJson))]
  | .metadata value => tagged "metadata" [("snapshot", snapshotJson value)]

private def pageResultJson : PageResult → String
  | .rejected => tagged "rejected"
  | .unknown => tagged "unknown"
  | .stale => tagged "stale"
  | .page value => tagged "page" [("page", pageJson value)]

structure PageInput where
  name : String
  source : SourceInput := large
  admission : Request := Examples.admission
  receipt : VerifiedReceipt := .metadata largeSnapshot
  cursor : Option Cursor := none

def pageInputs : List PageInput :=
  [ {name := "initial_bounded_page"}
  , {name := "continuation_page", cursor := some ⟨largeSnapshot, 64⟩}
  , {name := "wrong_cursor_offset", cursor := some ⟨largeSnapshot, 1⟩}
  , {name := "cursor_wrong_scope", cursor := some ⟨{largeSnapshot with admission := forkRequest}, 64⟩}
  , {name := "v1_is_not_metadata", receipt := .transcriptV1 admission largeSnapshot.documents}
  , {name := "late_terminal_behind_issued_cursor", source := lateAfter,
      receipt := .metadata lateSnapshot, cursor := some ⟨lateSnapshot, 64⟩}
  , {name := "membership_changed", source := Examples.source 66, cursor := some ⟨largeSnapshot, 64⟩}
  , { name := "projection_identity_changed"
    , source := { large with
        projections := large.projections.map (fun value => if value.tool = 1 then
          {value with observation := some 9999} else value) }
    , cursor := some ⟨largeSnapshot, 64⟩ }
  , { name := "projection_content_changed"
    , source := { large with
        projections := large.projections.map (fun value => if value.tool = 1 then
          {value with changes := [{(change 1).metadata with path := "other.rs"}]} else value) } }
  , { name := "current_owner_changed"
    , source := { large with
        projections := large.projections.map (fun value =>
          if value.tool = 1 then {value with owner := "foreign-node"} else value) }
    , cursor := some ⟨largeSnapshot, 64⟩ }
  , {name := "revoked_before_page", source := {large with catalog :=
      {large.catalog with verifiedActiveMemberships := []}}}
  , {name := "incomplete_read_cannot_certify", source := {large with boundary := .incomplete}}
  , {name := "valid_empty_inventory", source := source 0, receipt := .metadata (getSnapshot (source 0))}
  , {name := "legacy_unknown_coverage", source := {source 1 with projections := []},
      receipt := .metadata (getSnapshot {source 1 with projections := []})}
  , {name := "one_capture_exceeds_one_page", source := oneLargeCapture,
      receipt := .metadata (getSnapshot oneLargeCapture)} ]

private def pageInputJson (value : PageInput) : String := object
  [("name", jsonString value.name), ("source", sourceJson value.source),
   ("admission", hydrationRequestJson value.admission), ("receipt", receiptJson value.receipt),
   ("cursor", optional cursorJson value.cursor),
   ("expected", pageResultJson (page value.source.model value.admission value.receipt value.cursor))]

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

structure IssuanceInput where
  name : String
  envelope : PageEnvelope := firstEnvelope
  before : Bool := true
  after : Bool := true

private def issuanceInputs : List IssuanceInput :=
  [ {name := "current_authority"}
  , {name := "revoked_before_signing", before := false}
  , {name := "revoked_during_signing", after := false}
  , {name := "revoked_both", before := false, after := false}
  , {name := "foreign_signer", envelope := {firstEnvelope with signer := "node-2"}}
  , {name := "empty_nonce", envelope := {firstEnvelope with requestKey := ""}}
  , {name := "malformed_page", envelope := {firstEnvelope with page := {firstEnvelope.page with entries := []}}}
  , {name := "unknown_coverage", envelope := unknownEnvelope} ]

def issuanceCasesJson : String := jsonArray (issuanceInputs.map fun value => object
  [("name", jsonString value.name), ("envelope", envelopeJson value.envelope),
   ("before", boolString value.before), ("after", boolString value.after),
   ("expected", boolString (mayIssue value.envelope.page value.envelope.requestKey
     value.envelope.signer value.before value.after))])

private def demandJson (value : PageDemand) : String := object
  [("admission", hydrationRequestJson value.admission), ("attempt", jsonString value.attempt),
   ("cursor", optional pageCursorJson value.cursor)]

private def initialDemand : PageDemand := ⟨admission, "attempt-1", none⟩

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
private def continuingDemand : PageDemand := ⟨admission, "attempt-1", some ⟨firstEnvelope.page.binding, 64⟩⟩

def demandCasesJson : String :=
  let demands := [initialDemand, continuingDemand,
    {initialDemand with attempt := ""},
    {initialDemand with admission := forkRequest},
    {continuingDemand with cursor := some ⟨firstEnvelope.page.binding, 0⟩},
    {continuingDemand with cursor := some ⟨firstEnvelope.page.binding, 1⟩},
    {continuingDemand with cursor := some ⟨firstEnvelope.page.binding, 128⟩},
    {initialDemand with attempt := "attempt-2"}]
  jsonArray (demands.zipIdx |>.map fun (value, index) => object
    [("name", jsonString ("demand_" ++ toString index)), ("demand", demandJson value),
     ("well_formed", boolString value.wellFormed),
     ("same_initial", boolString (sameDemand value initialDemand)),
     ("initial_page", wirePageJson firstEnvelope.page),
     ("matches_initial_page", boolString (value.matchesPage firstEnvelope.page true))])


private def replyJson : Reply → String
  | .page value => object [("kind", jsonString "page"), ("page", wirePageJson value)]
  | .refused reason => object [("kind", jsonString "refused"), ("reason", refusalReasonJson reason)]

structure RefusalInput where
  name : String
  actual : PageDemand := initialDemand
  version : Bool := true
  nonceMatches : Bool := true
  signer : String := admission.node
  verified : Bool := true

private def refusalInputs : List RefusalInput :=
  [ {name := "valid_refusal"}
  , {name := "wrong_attempt", actual := {initialDemand with attempt := "attempt-2"}}
  , {name := "wrong_cursor", actual := continuingDemand}
  , {name := "wrong_session", actual := {initialDemand with admission := forkRequest}}
  , {name := "wrong_version", version := false}
  , {name := "wrong_nonce", nonceMatches := false}
  , {name := "foreign_signer", signer := "node-2"}
  , {name := "invalid_signature", verified := false} ]

def refusalCasesJson : String := jsonArray (refusalInputs.map fun value => object
  [("name", jsonString value.name), ("expected_demand", demandJson initialDemand),
   ("actual_demand", demandJson value.actual), ("version", boolString value.version),
   ("nonce_matches", boolString value.nonceMatches), ("signer", jsonString value.signer),
   ("verified", boolString value.verified),
   ("expected", boolString (acceptsRefusal initialDemand value.actual value.version
      value.nonceMatches value.signer value.verified))])

structure ReplyJournalInput where
  name : String
  prior : Option Reply := none
  candidate : Reply := .page firstEnvelope.page
  write : TerminalWriteResult := .committed

private def replyJournalInputs : List ReplyJournalInput :=
  [ {name := "first_page"}
  , {name := "first_stale", candidate := .refused .stale}
  , {name := "first_rejected", candidate := .refused .rejected}
  , {name := "first_unavailable", candidate := .refused .unavailable}
  , {name := "rollback_page", write := .failed}
  , {name := "rollback_refusal", candidate := .refused .stale, write := .failed}
  , {name := "retained_page", prior := some (.page firstEnvelope.page), candidate := .refused .stale}
  , {name := "retained_stale", prior := some (.refused .stale)}
  , {name := "retained_rejection", prior := some (.refused .rejected)}
  , {name := "retained_unavailable", prior := some (.refused .unavailable), write := .failed} ]

def replyJournalCasesJson : String := jsonArray (replyJournalInputs.map fun value => object
  [("name", jsonString value.name), ("demand", demandJson initialDemand),
   ("prior", optional replyJson value.prior), ("candidate", replyJson value.candidate),
   ("committed", boolString (value.write == .committed)),
   ("expected", optional replyJson (recordReply value.prior value.candidate value.write))])

structure PreparationInput where
  name : String
  prior : Option WirePage := none
  candidate : WirePage := firstEnvelope.page
  write : TerminalWriteResult := .committed
  demand : PageDemand := initialDemand

private def changedPreparedPage : WirePage :=
  (makePage (getSnapshot lateAfter) 0).toWire "changed-snapshot-digest"

private def preparationInputs : List PreparationInput :=
  [ {name := "first_preparation"}
  , {name := "rollback_preparation", write := .failed}
  , {name := "retry_preparation", prior := some firstEnvelope.page}
  , {name := "membership_changed_after_preparation", prior := some firstEnvelope.page,
      candidate := changedPreparedPage}
  , {name := "rollback_retry_preserves_preparation", prior := some firstEnvelope.page,
      candidate := changedPreparedPage, write := .failed}
  , {name := "new_attempt_selects_current_snapshot", candidate := changedPreparedPage,
      demand := {initialDemand with attempt := "attempt-2"}} ]

def preparationCasesJson : String := jsonArray (preparationInputs.map fun value => object
  [("name", jsonString value.name), ("demand", demandJson value.demand),
   ("prior", optional wirePageJson value.prior), ("candidate", wirePageJson value.candidate),
   ("committed", boolString (value.write == .committed)),
   ("expected", optional wirePageJson (preparePage value.prior value.candidate value.write)),
   ("unprepared_continuation", pageResultJson (page lateAfter.model admission
      (.metadata largeSnapshot) (some ⟨largeSnapshot, 64⟩)))])

private inductive ClientAction where
  | resume (candidate : String)
  | restart (attempt : String)
  | page (attempt : String) (envelope : PageEnvelope) (nonceMatches : Bool := true)
  | refused (attempt : String) (reason : RefusalReason) (verified : Bool := true)

private def clientAction (state : FileInventory.ClientProgress) : ClientAction → Option FileInventory.ClientProgress
  | .resume candidate => some (resumeClient (some state) state.active.receiver.admission candidate)
  | .restart attempt => restartClient state attempt
  | .page attempt envelope nonce =>
      let key := attempt ++ "/" ++ toString envelope.page.offset
      clientPage state attempt key {envelope with requestKey := key} nonce
  | .refused attempt reason verified => clientRefusal state attempt reason verified

private def clientActionJson : ClientAction → String
  | .resume candidate => tagged "resume" [("attempt", jsonString candidate)]
  | .restart attempt => tagged "restart" [("attempt", jsonString attempt)]
  | .page attempt envelope nonce => tagged "page"
      [("attempt", jsonString attempt), ("page", wirePageJson envelope.page),
       ("nonce_matches", boolString nonce)]
  | .refused attempt reason verified => tagged "refused"
      [("attempt", jsonString attempt), ("reason", refusalReasonJson reason),
       ("verified", boolString verified)]

def clientScopeCasesJson : String :=
  let otherPhysical := {admission with nativeSession := admission.nativeSession + 1}
  let otherRequester := {admission with requester := "other-requester"}
  let cases : List (String × String × String × String × List Request) :=
    [("missing", admission.peer, admission.node, admission.session, []),
     ("exact", admission.peer, admission.node, admission.session, [admission]),
     ("foreign_node", admission.peer, "other-node", admission.session, [admission]),
     ("foreign_peer", "other-peer", admission.node, admission.session, [admission]),
     ("foreign_session", admission.peer, admission.node, "other-session", [admission]),
     ("ambiguous_physical", admission.peer, admission.node, admission.session, [admission, otherPhysical]),
     ("ambiguous_requester", admission.peer, admission.node, admission.session, [admission, otherRequester]),
     ("invalid", "", admission.node, admission.session, [admission])]
  jsonArray (cases.map fun (name, peer, node, session, rows) =>
    object [("name", jsonString name), ("peer", jsonString peer), ("node", jsonString node),
      ("session", jsonString session), ("rows", jsonArray (rows.map hydrationRequestJson)),
      ("expected", match selectRetainedScope peer node session rows with
        | .error reason => tagged "error" [("reason", jsonString reason)]
        | .ok none => tagged "missing"
        | .ok (some selected) => tagged "selected" [("admission", hydrationRequestJson selected)])])

private def clientSummaryJson (state : FileInventory.ClientProgress) : String := object
  [("attempt", jsonString state.active.id),
   ("pages", toString state.active.receiver.pages.length),
   ("visible_entries", optional (fun page => toString page.envelope.page.entries.length)
      state.active.receiver.pages.head?),
   ("next_received_offset", optional (fun page => toString page.envelope.page.offset)
      (state.active.receiver.pages.drop 1).head?),
   ("last_page_entries", optional (fun page => toString page.envelope.page.entries.length)
      state.active.receiver.pages.getLast?),
   ("retired", jsonArray (state.retired.map jsonString)),
   ("complete", boolString state.active.receiver.complete),
   ("known", boolString (knownInventory state.active.receiver)),
   ("refusal", optional refusalReasonJson state.active.refusal),
   ("pending_offset", optional (fun demand => toString
      (demand.cursor.map (·.offset) |>.getD 0)) state.active.pending),
   ("previous_pages", optional (fun prior => toString prior.receiver.pages.length) state.previous)]

private def clientStepsJson (state : FileInventory.ClientProgress) : List (ClientAction × TerminalWriteResult) → List String
  | [] => []
  | (action, write) :: rest =>
      let candidate := clientAction state action
      let next := commitClient state (candidate.getD state) write
      object [("action", clientActionJson action), ("committed", boolString (write == .committed)),
        ("accepted", boolString candidate.isSome), ("expected", clientSummaryJson next)] ::
        clientStepsJson next rest

def clientProgressCasesJson : String :=
  let scripts : List (String × List (ClientAction × TerminalWriteResult)) :=
    [("reload_retry_refresh_and_late_reply", [
        (.resume "unused-candidate", .committed),
        (.page "attempt-1" firstEnvelope, .committed),
        (.page "attempt-1" firstEnvelope false, .committed),
        (.page "attempt-1" firstEnvelope, .committed),
        (.page "attempt-1" secondEnvelope, .committed),
        (.page "attempt-1" secondEnvelope, .committed),
        (.restart "attempt-2", .committed),
        (.resume "attempt-1", .committed),
        (.restart "attempt-2", .committed),
        (.page "attempt-1" firstEnvelope, .committed),
        (.refused "attempt-2" .stale, .committed),
        (.refused "attempt-2" .stale, .committed),
        (.restart "attempt-3", .committed),
        (.page "attempt-3" firstEnvelope, .committed),
        (.page "attempt-3" secondEnvelope, .failed),
        (.page "attempt-3" secondEnvelope, .committed),
        (.restart "attempt-1", .committed),
        (.restart "attempt-2", .committed)]),
     ("refusal_is_not_empty", [
        (.refused "attempt-1" .unavailable, .committed),
        (.page "attempt-1" firstEnvelope, .committed),
        (.refused "attempt-1" .rejected, .committed)]),
     ("unverified_and_wrong_nonce", [
        (.refused "attempt-1" .rejected false, .committed),
        (.page "attempt-1" firstEnvelope false, .committed),
        (.page "attempt-1" secondEnvelope, .committed),
        (.restart "", .committed),
        (.page "attempt-1" firstEnvelope, .failed),
        (.page "attempt-1" firstEnvelope, .committed)])]
  jsonArray (scripts.map fun (name, steps) => object
    [("name", jsonString name), ("admission", hydrationRequestJson admission),
     ("attempt", jsonString "attempt-1"),
     ("steps", jsonArray (clientStepsJson (beginClient admission "attempt-1") steps))])

def operatorOwnerCasesJson : String :=
  let inputs : List (String × String × String × String × String) :=
    [("native_owner", "node-1", "node-1", "node-1", "node-1"),
     ("anonymous", "node-1", "", "node-1", "node-1"),
     ("foreign_authenticated_actor", "node-1", "node-2", "node-1", "node-1"),
     ("different_requested_source", "node-1", "node-1", "node-1", "node-2"),
     ("different_signing_identity", "node-1", "node-1", "node-2", "node-1")]
  let source : SourceInput := {Examples.source 0 with catalog :=
    {catalog [] with appliedPairingRoutes := [], verifiedActiveMemberships := []}}
  jsonArray (inputs.map fun (name, owner, actor, signer, node) =>
    let request := {admission with node := node}
    let result := operatorSnapshot source.model owner actor signer request
    object [("name", jsonString name), ("owner", jsonString owner),
      ("actor", jsonString actor), ("signer", jsonString signer),
      ("admission", hydrationRequestJson request), ("source", sourceJson source),
      ("allowed", boolString (operatorAdmitted owner actor signer request)),
      ("expected", selectionResultJson result),
      ("page", match result with
        | .error _ => "null"
        | .ok current => wirePageJson ((makePage current 0).toWire "operator-snapshot")),
      ("wrong_physical_refusal", match operatorSnapshot source.model owner actor signer
          {request with nativeSession := request.nativeSession + 1} with
        | .error error => refusalReasonJson error.refusal
        | .ok _ => "null")])

def completionCasesJson : String :=
  let candidates := [Reply.page firstEnvelope.page, .refused .stale,
    .refused .rejected, .refused .unavailable]
  jsonArray (candidates.flatMap fun candidate =>
    [true, false].flatMap fun before => [true, false].flatMap fun after =>
    [DeliveryResult.confirmed, .indeterminate].flatMap fun delivery =>
    [TerminalWriteResult.committed, .failed].map fun write => object
      [("candidate", replyJson candidate), ("before", boolString before), ("after", boolString after),
       ("confirmed", boolString (delivery == .confirmed)), ("committed", boolString (write == .committed)),
       ("expected", optional replyJson (completeReply candidate before after delivery write))])

def pageGrantCasesJson : String := jsonArray (refusalInputs.map fun value => object
  [("name", jsonString (if value.nonceMatches then value.name else "empty_document")), ("expected_demand", demandJson initialDemand),
   ("actual_demand", demandJson value.actual), ("version", boolString value.version),
   ("document", jsonString (if value.nonceMatches then "page-document" else "")),
   ("signer", jsonString value.signer), ("verified", boolString value.verified),
   ("expected", boolString (acceptsPageGrant initialDemand value.actual value.version
      (if value.nonceMatches then "page-document" else "") value.signer value.verified))])

private def receivedJson (value : ReceivedPage) : String := object
  [("request_key", jsonString value.requestKey), ("envelope", envelopeJson value.envelope)]

private def receiverJson (value : Receiver) : String := object
  [("admission", hydrationRequestJson value.admission), ("pages", jsonArray (value.pages.map receivedJson))]

private def receiverResultJson (value : Receiver) : String := object
  [("state", receiverJson value), ("complete", boolString value.complete),
   ("known_inventory", boolString (knownInventory value)),
   ("validated", optional (fun prior => object
      [("binding", bindingJson prior.binding), ("entries", jsonArray (prior.entries.map entryJson)),
       ("complete", boolString prior.complete)]) value.validated)]

private def receiverInputJson (value : Examples.ReceiverInput) : String := object
  [("name", jsonString value.name), ("state", receiverJson value.state),
   ("request_key", jsonString value.requestKey), ("envelope", envelopeJson value.envelope),
   ("expected", optional receiverResultJson (receive value.state value.requestKey value.envelope))]

def receiverCasesJson : String := jsonArray (receiverInputs.map receiverInputJson)


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

def casesJson : String := object
  [("selection", jsonArray ((selectionCases ++ SessionHydration.ReaderExamples.cases).map selectionCaseJson)),
   ("publication", publicationCasesJson),
   ("fork", object [("source", sourceJson forkSource), ("admission", hydrationRequestJson forkRequest),
      ("expected", selectionJson forkSource forkRequest)]),
   ("paging", jsonArray (pageInputs.map pageInputJson)),
   ("receiver", receiverCasesJson)]

end Conformance.FileInventoryContracts
