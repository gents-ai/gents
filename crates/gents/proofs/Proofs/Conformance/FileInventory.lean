import Proofs.Observe.FileInventory.Cases
import Proofs.Observe.FileInventory.OrderCases
import Proofs.Observe.FileInventory.ReaderCases
import Proofs.Conformance.Contracts.Json.FileInventoryClosure
import Proofs.Conformance.Contracts.Json.NativeExecution
import Proofs.Conformance.Family

namespace Conformance.FileInventoryContracts

open FileInventory
open FileInventory.Examples
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
  [("scope", object [("requester", jsonString value.scope.requester), ("node", jsonString value.scope.node),
      ("session", jsonString value.scope.session),
      ("native_session", toString value.scope.nativeSession)]),
   ("source", sessionOwnerJson value.source), ("visible", boolString value.visible)]

private def catalogJson (value : CatalogInput) : String := object
  [("sessions", jsonArray (value.sessions.map sessionOwnerJson)),
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
    | .readerBound scope => object [("requester", jsonString scope.requester), ("node", jsonString scope.node),
        ("session", jsonString scope.session), ("native_session", toString scope.nativeSession)]
    | _ => "null"),
   ("boundary", jsonString (boundaryName value.boundary)),
   ("session_owner", jsonString value.sessionOwner)]

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

def readerCasesJson : String := jsonArray ((FileInventory.ReaderExamples.cases.filter fun value =>
  ["shared_native_root_read", "shared_authorized_empty_root",
   "shared_root_physical_identity_changed", "shared_missing_capture_is_not_empty_complete",
   "shared_unreadable_projection_owner_is_not_trusted"].contains value.name).map fun value =>
  object [("name", jsonString value.name), ("source", sourceJson value.source),
    ("expected", selectionResultJson (directSnapshot value.source.model admission.requester admission)),
    ("coverage", match directSnapshot value.source.model admission.requester admission with
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

private def directSource (visible : Bool) : SourceInput :=
  let current := Examples.source 1
  let root := { ownedSession admission with requester := "session-creator" }
  { current with
    boundary := .readerBound admission.authorizationScope
    catalog := { current.catalog with
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

deriving instance Lean.ToJson for Order.Window

structure OrderResolutionInput where
  name : String
  windows : List Order.Window
  deriving Lean.ToJson

@[conformance_family]
def orderResolution : Family OrderResolutionInput (Option (List Nat)) where
  inputs := Order.Examples.resolutionCases.map fun (name, windows) => { name, windows }
  model := fun i => Order.resolve i.windows

deriving instance Lean.ToJson for FileImages.Scope
deriving instance Lean.ToJson for Coverage

private def failureOfName (name : String) : ImageFailure :=
  match name with
  | "read_failed" => .readFailed
  | "changed_during_read" => .changedDuringRead
  | "too_large" => .tooLarge
  | "unsupported_entry" => .unsupportedEntry
  | _ => .mutationIncomplete

structure ImageWire where
  kind : String
  contentHash : Option String
  reason : Option String
  deriving Lean.ToJson

def ImageWire.ofModel : ImageMetadata → ImageWire
  | .missing => ⟨"missing", none, none⟩
  | .file hash => ⟨"file", some hash, none⟩
  | .unavailable reason => ⟨"unavailable", none, some (failureName reason)⟩

def ImageWire.toModel (value : ImageWire) : ImageMetadata :=
  match value.kind, value.contentHash, value.reason with
  | "file", some hash, _ => .file hash
  | "unavailable", _, some reason => .unavailable (failureOfName reason)
  | _, _, _ => .missing

structure ChangeWire where
  root : String
  path : String
  before : ImageWire
  after : ImageWire
  deriving Lean.ToJson

def ChangeWire.ofModel (value : Change) : ChangeWire :=
  ⟨value.root, value.path, .ofModel value.before, .ofModel value.after⟩

def ChangeWire.toModel (value : ChangeWire) : Change :=
  ⟨value.root, value.path, value.before.toModel, value.after.toModel⟩

structure PositionWire where
  kind : String
  index : Option Nat
  entries : Option Nat
  deriving Lean.ToJson

def PositionWire.ofModel : Order.Position → PositionWire
  | .unresolved => ⟨"unresolved", none, none⟩
  | .ordered index entries => ⟨"ordered", some index, some entries⟩

def PositionWire.toModel (value : PositionWire) : Order.Position :=
  match value.index, value.entries with
  | some index, some entries => .ordered index entries
  | _, _ => .unresolved

structure EntryWire where
  tool : Nat
  request : Nat
  scope : FileImages.Scope
  projection : Nat
  observation : Nat
  change : ChangeWire
  requestCoverage : Option Coverage
  order : PositionWire
  deriving Lean.ToJson

def EntryWire.ofModel (value : Entry) : EntryWire :=
  ⟨value.tool, value.request, value.scope, value.projection, value.observation,
    .ofModel value.change, value.requestCoverage, .ofModel value.order⟩

def EntryWire.toModel (value : EntryWire) : Entry :=
  ⟨value.tool, value.request, value.scope, value.projection, value.observation,
    value.change.toModel, value.order.toModel, value.requestCoverage⟩

structure OrderEntryInput where
  name : String
  entries : List EntryWire
  deriving Lean.ToJson

@[conformance_family]
def orderEntry : Family OrderEntryInput Bool where
  inputs := Order.Examples.entryCases.map fun (name, entries) =>
    { name, entries := entries.map EntryWire.ofModel }
  model := fun i => coherentEntries (i.entries.map EntryWire.toModel)

theorem entry_wire_round_trips :
    Order.Examples.entryCases.all (fun (_, entries) =>
      (entries.map EntryWire.ofModel).map EntryWire.toModel == entries) = true := by
  native_decide

deriving instance Lean.ToJson for CanonicalOutput.DependencyDenial

structure DocumentKeyWire where
  collection : String
  id : Nat
  deriving Lean.ToJson

def DocumentKeyWire.ofModel (key : CanonicalOutput.Hydration.DocumentKey) : DocumentKeyWire :=
  ⟨hydrationCollectionName key.collection, key.id⟩

structure ToolWire where
  id : Nat
  request : Nat
  scope : FileImages.Scope
  agentDid : String
  accepted : Bool
  state : String
  deriving Lean.ToJson

def ToolWire.ofModel (value : Tool) : ToolWire :=
  ⟨value.id, value.request, value.scope, value.agentDid, value.accepted, value.state.toDefraDB⟩

structure ProjectionWire where
  id : Nat
  observation : Option Nat
  tool : Nat
  request : Nat
  scope : FileImages.Scope
  owner : String
  changes : List ChangeWire
  verifiedUnchanged : Bool
  deriving Lean.ToJson

def ProjectionWire.ofModel (value : Projection) : ProjectionWire :=
  ⟨value.id, value.observation, value.tool, value.request, value.scope, value.owner,
    value.changes.map ChangeWire.ofModel, value.verifiedUnchanged⟩

structure CaptureWire where
  kind : String
  projection : Option ProjectionWire
  deriving Lean.ToJson

def CaptureWire.ofModel : Capture → CaptureWire
  | .pending => ⟨"pending", none⟩
  | .unknown => ⟨"unknown", none⟩
  | .observed projection => ⟨"observed", some (.ofModel projection)⟩

structure ToolFactWire where
  tool : ToolWire
  capture : CaptureWire
  deriving Lean.ToJson

structure SnapshotWire where
  admission : FileInventory.Request
  documents : List DocumentKeyWire
  tools : List ToolFactWire
  ordering : List (Option Order.Window)
  deriving Lean.ToJson

def SnapshotWire.ofModel (value : Snapshot) : SnapshotWire :=
  ⟨value.admission, value.documents.map DocumentKeyWire.ofModel,
    value.tools.map fun fact => ⟨.ofModel fact.tool, .ofModel fact.capture⟩, value.ordering⟩

structure PublicationWire where
  kind : String
  generation : Option Nat
  call : Option Nat
  origin : Option Nat
  deriving Lean.ToJson

def PublicationWire.ofModel : CanonicalOutput.MessagePublication → PublicationWire
  | .requestExecution generation => ⟨"request_execution", some generation, none, none⟩
  | .requestRecovery generation => ⟨"request_recovery", some generation, none, none⟩
  | .toolDelivery call => ⟨"tool_delivery", none, some call, none⟩
  | .fork origin => ⟨"fork", none, none, some origin⟩

private def outcomeName : CanonicalOutput.Outcome → String
  | .complete => "complete"
  | .partial => "partial"

private def roleName : CanonicalOutput.MessageRole → String
  | .system => "system"
  | .user => "user"
  | .assistant => "assistant"

structure HeaderWire where
  id : Nat
  session : Nat
  request : Option Nat
  origin : Option Nat
  refs : List CanonicalOutput.PayloadRef
  outcome : String
  role : String
  publication : PublicationWire
  deriving Lean.ToJson

def HeaderWire.ofModel (value : CanonicalOutput.Header) : HeaderWire :=
  ⟨value.id, value.session, value.request, value.origin, value.refs, outcomeName value.outcome,
    roleName value.role, .ofModel value.publication⟩

structure PresentationPartWire where
  kind : String
  start : Option Nat
  stop : Option Nat
  bytes : List Nat
  deriving Lean.ToJson

structure PresentationWire where
  kind : String
  parts : List PresentationPartWire
  deriving Lean.ToJson

def PresentationWire.ofModel : CanonicalOutput.Presentation → PresentationWire
  | .full => ⟨"full", []⟩
  | .composed parts => ⟨"composed", parts.map fun part => match part with
      | .range start stop => ⟨"range", some start, some stop, []⟩
      | .literal bytes => ⟨"literal", none, none, bytes.map (·.toNat)⟩⟩

structure PayloadSpecWire where
  reference : CanonicalOutput.PayloadRef
  presentation : PresentationWire
  deriving Lean.ToJson

def PayloadSpecWire.ofModel (value : CanonicalOutput.PayloadSpec) : PayloadSpecWire :=
  ⟨value.reference, .ofModel value.presentation⟩

structure ResultPartWire where
  kind : String
  payload : Option PayloadSpecWire
  deriving Lean.ToJson

/-- Media parts carry only their kind: no file inventory case holds one, and
the native adapter refuses them. -/
def ResultPartWire.ofModel : CanonicalOutput.ResultPart CanonicalOutput.PayloadSpec → ResultPartWire
  | .text payload => ⟨"text", some (.ofModel payload)⟩
  | .media _ => ⟨"media", none⟩

structure BlockWire where
  kind : String
  payload : Option PayloadSpecWire
  docId : Option Nat
  id : Option String
  callId : Option String
  name : Option String
  arguments : Option PayloadSpecWire
  signature : Option String
  additionalParams : Option String
  parts : List ResultPartWire
  deriving Lean.ToJson

/-- Reasoning and media blocks carry only their kind: no file inventory case
holds one, and the native adapter refuses them. -/
def BlockWire.ofModel : CanonicalOutput.MessageBlock CanonicalOutput.PayloadSpec → BlockWire
  | .text payload => ⟨"text", some (.ofModel payload), none, none, none, none, none, none, none, []⟩
  | .reasoning _ _ => ⟨"reasoning", none, none, none, none, none, none, none, none, []⟩
  | .toolCall docId id callId name arguments signature additionalParams =>
      ⟨"tool_call", none, some docId, some id, callId, some name, some (.ofModel arguments),
        signature, additionalParams, []⟩
  | .toolResult docId id callId parts =>
      ⟨"tool_result", none, some docId, some id, callId, none, none, none, none,
        parts.map ResultPartWire.ofModel⟩
  | .media _ => ⟨"media", none, none, none, none, none, none, none, none, []⟩

structure MessageWire where
  header : HeaderWire
  key : String
  sequence : Nat
  nativeId : Option String
  nativeRequestId : Option String
  blocks : List BlockWire
  createdAt : Nat
  deriving Lean.ToJson

def MessageWire.ofModel (value : CanonicalOutput.MessageEnvelope) : MessageWire :=
  ⟨.ofModel value.header, value.key, value.sequence, value.nativeId, value.nativeRequestId,
    value.blocks.map BlockWire.ofModel, value.createdAt⟩

structure SegmentSourceWire where
  kind : String
  auxiliaryKind : Option String
  scope : Option Nat
  turn : Option Nat
  attempt : Option Nat
  call : Option Nat
  key : Option Nat
  deriving Lean.ToJson

private def auxiliaryName : CanonicalOutput.AuxiliaryKind → String
  | .compaction => "compaction"
  | .compactionFallback => "compaction_fallback"
  | .title => "title"
  | .accessReview => "access_review"

def SegmentSourceWire.ofModel : CanonicalOutput.Source → SegmentSourceWire
  | .provider scope turn attempt => ⟨"provider", none, some scope, some turn, some attempt, none, none⟩
  | .auxiliary kind scope turn attempt =>
      ⟨"auxiliary", some (auxiliaryName kind), some scope, some turn, some attempt, none, none⟩
  | .tool call => ⟨"tool", none, none, none, none, some call, none⟩
  | .toolEffectArguments call => ⟨"tool_effect_arguments", none, none, none, none, some call, none⟩
  | .authored key => ⟨"authored", none, none, none, none, none, some key⟩

structure CoordinateWire where
  request : Nat
  source : SegmentSourceWire
  deriving Lean.ToJson

structure WriterWire where
  kind : String
  generation : Option Nat
  call : Option Nat
  deriving Lean.ToJson

def WriterWire.ofModel : CanonicalOutput.Writer → WriterWire
  | .request generation => ⟨"request", some generation, none⟩
  | .tool call => ⟨"tool", none, some call⟩

private def payloadKindName : CanonicalOutput.PayloadKind → String
  | .text => "text" | .reasoning => "reasoning" | .summary => "summary"
  | .signature => "signature" | .reasoningStop => "reasoning_stop"
  | .encrypted => "encrypted" | .redacted => "redacted"
  | .arguments => "arguments" | .toolOutput => "tool_output"
  | .media => "media"

private def mediaKindName : CanonicalOutput.MediaKind → String
  | .image => "image" | .audio => "audio" | .video => "video" | .document => "document"

structure DeclarationWire where
  block : Nat
  part : Nat
  kind : String
  tool : Option CanonicalOutput.ToolIdentity
  mediaKind : Option String
  deriving Lean.ToJson

def DeclarationWire.ofModel (value : CanonicalOutput.Declaration) : DeclarationWire :=
  ⟨value.block, value.part, payloadKindName value.kind, value.tool, value.mediaKind.map mediaKindName⟩

structure RunWire where
  stream : Nat
  bytes : Nat
  declaration : Option DeclarationWire
  deriving Lean.ToJson

structure FlushWire where
  ordinal : Nat
  runs : List RunWire
  payload : List Nat
  deriving Lean.ToJson

def FlushWire.ofModel (value : CanonicalOutput.Flush) : FlushWire :=
  ⟨value.ordinal, value.runs.map fun run => ⟨run.stream, run.bytes, run.declaration.map .ofModel⟩,
    value.payload.map (·.toNat)⟩

structure CloseWire where
  kind : String
  outcome : Option String
  segments : Option Nat
  streamBytes : List Nat
  deriving Lean.ToJson

def CloseWire.ofModel : CanonicalOutput.Closure → CloseWire
  | .closed outcome segments streamBytes => ⟨"closed", some (outcomeName outcome), some segments, streamBytes⟩
  | .retracted => ⟨"retracted", none, none, []⟩

structure SegmentWire where
  id : Nat
  coordinate : CoordinateWire
  writer : WriterWire
  flush : Option FlushWire
  close : Option CloseWire
  createdAt : Nat
  deriving Lean.ToJson

def SegmentWire.ofModel (value : CanonicalOutput.Segment) : SegmentWire :=
  ⟨value.id, ⟨value.coordinate.request, .ofModel value.coordinate.source⟩, .ofModel value.writer,
    value.flush.map .ofModel, value.close.map .ofModel, value.createdAt⟩

structure AccessWire where
  key : DocumentKeyWire
  state : String
  requester : String
  node : String
  session : String
  nativeSession : Nat
  deriving Lean.ToJson

def AccessWire.ofModel (value : CanonicalOutput.Hydration.ProvenanceAccess) : AccessWire :=
  ⟨.ofModel value.key, accessStateName value.state, value.scope.requester, value.scope.node,
    value.scope.session, value.scope.nativeSession⟩

structure SelectionWire where
  kind : String
  id : Option Nat
  deriving Lean.ToJson

structure RequirementWire where
  request : Nat
  session : Nat
  selection : Option SelectionWire
  deriving Lean.ToJson

structure ClosureWire where
  request : FileInventory.Request
  bases : List DocumentKeyWire
  roots : List Nat
  requirements : List RequirementWire
  messages : List MessageWire
  segments : List SegmentWire
  access : List AccessWire
  deniedHeaders : List Nat
  deniedSegments : List Nat
  dependencyDenials : List CanonicalOutput.DependencyDenial
  effects : List CanonicalOutput.Hydration.EffectRow
  deriving Lean.ToJson

def ClosureWire.ofModel (value : FileInventory.ClosureInput) : ClosureWire :=
  ⟨value.request, value.bases.map (DocumentKeyWire.ofModel ·.key), value.roots,
    value.requirements.map (fun row => ⟨row.request, row.session, row.selection.map fun
      | .noMessage => ⟨"no_message", none⟩
      | .message id => ⟨"message", some id⟩⟩),
    value.messages.map MessageWire.ofModel, value.segments.map SegmentWire.ofModel,
    value.access.map AccessWire.ofModel, value.deniedHeaders, value.deniedSegments,
    value.dependencyDenials, value.effects⟩

structure OrderWindowInput where
  name : String
  snapshot : SnapshotWire
  closure : ClosureWire
  tool : ToolWire
  deriving Lean.ToJson

def OrderWindowInput.ofCase : String × Snapshot × FileInventory.ClosureInput × Tool → OrderWindowInput
  | (name, selected, closure, tool) =>
    { name, snapshot := .ofModel selected, closure := .ofModel closure, tool := .ofModel tool }

example : (Order.Examples.windowCases.map (·.1)).Nodup := by native_decide

def orderWindowOf (name : String) : Option Order.Window :=
  (Order.Examples.windowCases.find? (·.1 == name)).bind fun (_, selected, closure, tool) =>
    Order.window selected closure tool

/-- The wire carries each case to the native adapter; the model evaluates the
case its unique name selects. -/
@[conformance_family]
def orderWindow : Family OrderWindowInput (Option Order.Window) where
  inputs := Order.Examples.windowCases.map OrderWindowInput.ofCase
  model := fun i => orderWindowOf i.name

/-- A served node publishes plugin effects under their parent tool, and an
effect has no canonical invocation and delivery of its own, so this is the one
window case its scenario can build. -/
def pluginEffectWindowCases : List (String × Snapshot × FileInventory.ClosureInput × Tool) :=
  Order.Examples.windowCases.filter (·.1 ∈ ["accepted_effect_without_own_publication"])

/-- Whether the scenario's inventory entries carry a canonical order. -/
@[conformance_family]
def orderWindowPluginEffect : Family OrderWindowInput Bool where
  inputs := pluginEffectWindowCases.map OrderWindowInput.ofCase
  model := fun i => (orderWindowOf i.name).isSome

structure OrderComparisonInput where
  name : String
  snapshot : SnapshotWire
  closure : ClosureWire
  node : String
  root : String
  path : String
  deriving Lean.ToJson

def OrderComparisonInput.ofCase :
    String × Snapshot × FileInventory.ClosureInput × String × String × String → OrderComparisonInput
  | (name, selected, closure, node, root, path) =>
    { name, snapshot := .ofModel selected, closure := .ofModel closure, node, root, path }

def comparisonSnapshotOf (name : String) : Option Snapshot :=
  (Order.Examples.comparisonCases.find? (·.1 == name)).map (·.2.1)

def orderComparisonOf (name : String) : Option (List Nat) :=
  (Order.Examples.comparisonCases.find? (·.1 == name)).bind
    fun (_, selected, closure, node, root, path) => Order.comparison selected closure node root path

def comparisonInputs (names : List String) : List OrderComparisonInput :=
  (Order.Examples.comparisonCases.filter (·.1 ∈ names)).map OrderComparisonInput.ofCase

/-- The wire carries each case to the native adapter; the model evaluates the
case its unique name selects. -/
@[conformance_family]
def orderComparison : Family OrderComparisonInput (Option (List Nat)) where
  inputs := Order.Examples.comparisonCases.map OrderComparisonInput.ofCase
  model := fun i => orderComparisonOf i.name

structure ToolCoverage where
  tool : Nat
  coverage : Coverage
  deriving Lean.ToJson

/-- The coverage of every selected tool's own scope and request. -/
@[conformance_family]
def requestCoverage : Family OrderComparisonInput (List ToolCoverage) where
  inputs := Order.Examples.comparisonCases.map OrderComparisonInput.ofCase
  model := fun i => ((comparisonSnapshotOf i.name).map fun selected =>
    selected.tools.map fun fact =>
      (⟨fact.tool.id, selected.requestCoverage fact.tool.scope fact.tool.request⟩ : ToolCoverage)).getD []

/-- The served-node fork scenario orders one resource in its parent, in a fork
cut before later parent work, and in a fork of both edits; these are the
comparison cases it builds. -/
@[conformance_family]
def forkResourceOrder : Family OrderComparisonInput (Option (List Nat)) where
  inputs := comparisonInputs
    ["sequential_resource", "forked_prefix_resource", "forked_repeated_resource"]
  model := fun i => orderComparisonOf i.name

structure EntryCoverage where
  tool : Nat
  coverage : Option Coverage
  deriving Lean.ToJson

/-- The served-node fork scenario adds one uncaptured tool on another request
of its parent session, so it builds the unknown-other-request case. -/
@[conformance_family]
def unknownRequestEntryCoverage : Family OrderComparisonInput (List EntryCoverage) where
  inputs := comparisonInputs ["unknown_other_request"]
  model := fun i => ((comparisonSnapshotOf i.name).map fun selected =>
    selected.entries.map fun entry => (⟨entry.tool, entry.requestCoverage⟩ : EntryCoverage)).getD []

/-- A served node restarts after a queued or a retried turn published two
sequential edits of one resource, so its turn scenario builds the sequential
case. -/
@[conformance_family]
def turnChronology : Family OrderComparisonInput (Option (List Nat)) where
  inputs := comparisonInputs ["sequential_resource"]
  model := fun i => orderComparisonOf i.name

structure ChronologyObservation where
  entries : Nat
  expected : Option (List Nat)
  deriving Lean.ToJson

/-- A served node publishes two sequential terminal edits of one resource, as a
foreground pair, behind a background receipt, or with a completion
notification; these are the three comparison cases it can build. -/
@[conformance_family]
def terminalChronology : Family OrderComparisonInput ChronologyObservation where
  inputs := comparisonInputs
    ["sequential_resource", "background_receipt_resource", "background_completion_reference"]
  model := fun i =>
    { entries := ((comparisonSnapshotOf i.name).map (·.entries.length)).getD 0
      expected := orderComparisonOf i.name }

/-- IDs, DIDs and image hashes are the existing models' abstract names. Native
adapters bind them to actual Defra-created physical IDs, authenticated key
identities and validated captures through their existing decode owners. Empty
or malformed native IDs cannot become modeled observations; abstract hash
labels cannot bypass the capture decoder's byte/hash checks. These are
executable inputs/outputs, not a production wire-format proposal. -/
def publicationCasesJson : String := jsonArray (publicationCases.map publicationCaseJson)

end Conformance.FileInventoryContracts
