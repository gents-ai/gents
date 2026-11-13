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

private def boundaryName : ReadBoundary → String
  | .atomicExhaustive => "atomic_exhaustive"
  | .readerBound _ => "reader_bound"
  | .incomplete => "incomplete"
  | .nonAtomic => "non_atomic"

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

deriving instance Lean.ToJson for SessionOwner
deriving instance Lean.ToJson for RefusalReason

structure ScopeWire where
  requester : String
  node : String
  session : String
  nativeSession : Nat
  deriving Lean.ToJson

def ScopeWire.ofModel (value : CanonicalOutput.Hydration.AuthorizationScope) : ScopeWire :=
  ⟨value.requester, value.node, value.session, value.nativeSession⟩

structure SessionReadWire where
  scope : ScopeWire
  source : SessionOwner
  visible : Bool
  deriving Lean.ToJson

structure CatalogWire where
  sessions : List SessionOwner
  sessionReads : List SessionReadWire
  closureInputs : List ClosureWire
  deriving Lean.ToJson

structure SourceWire where
  catalog : CatalogWire
  tools : List ToolWire
  projections : List ProjectionWire
  readScope : Option ScopeWire
  boundary : String
  sessionOwner : String
  deriving Lean.ToJson

def SourceWire.ofModel (value : SourceInput) : SourceWire :=
  { catalog :=
      { sessions := value.catalog.sessions
        sessionReads := value.catalog.sessionReads.map fun read =>
          ⟨.ofModel read.scope, read.source, read.visible⟩
        closureInputs := value.catalog.closureInputs.map ClosureWire.ofModel }
    tools := value.tools.map ToolWire.ofModel
    projections := value.projections.map ProjectionWire.ofModel
    readScope := match value.boundary with
      | .readerBound scope => some (.ofModel scope)
      | _ => none
    boundary := boundaryName value.boundary
    sessionOwner := value.sessionOwner }

structure DirectReaderInput where
  name : String
  actor : String
  admission : FileInventory.Request
  source : SourceWire
  deriving Lean.ToJson

/-- What the served node's direct reader shows: whether the read was accepted,
for the admitted physical root and for another one, and the first page's
binding coverage and entry count. -/
structure DirectReaderOutput where
  selected : Bool
  wrongPhysicalSelected : Bool
  coverage : Option Coverage
  entries : Option Nat
  deriving Lean.ToJson

example : (directCases.map (·.1)).Nodup := by native_decide

def directSourceOf (name : String) : SourceInput :=
  ((directCases.find? (·.1 == name)).map (·.2.2)).getD (directSource true)

/-- The wire carries each case to the native adapter; the model evaluates the
case its unique name selects. -/
@[conformance_family]
def directReaderSnapshot : Family DirectReaderInput DirectReaderOutput where
  inputs := directCases.map fun (name, actor, source) =>
    { name, actor, admission, source := .ofModel source }
  model := fun i =>
    let source := (directSourceOf i.name).model
    let result := directSnapshot source i.actor i.admission
    let firstPage := result.toOption.map fun current => makePage current 0
    { selected := result.toOption.isSome
      wrongPhysicalSelected := (directSnapshot source i.actor
        {i.admission with nativeSession := i.admission.nativeSession + 1}).toOption.isSome
      coverage := firstPage.map fun page => (page.toWire "direct-snapshot").binding.coverage
      entries := firstPage.map (·.entries.length) }

structure ForkInput where
  name : String
  source : SourceWire
  admission : FileInventory.Request
  deriving Lean.ToJson

structure ForkOutput where
  selected : Bool
  entries : Nat
  deriving Lean.ToJson

/-- The served-node fork scenario forks one completed, captured parent tool;
this is the one source it builds. -/
@[conformance_family]
def forkSnapshot : Family ForkInput ForkOutput where
  inputs := [{ name := "authorized_fork_origin", source := .ofModel forkSource, admission := forkRequest }]
  model := fun _ =>
    let result := snapshot forkSource.model forkRequest
    { selected := result.toOption.isSome
      entries := (result.toOption.map (·.entries.length)).getD 0 }

structure SelectionInput where
  name : String
  source : SourceWire
  admission : FileInventory.Request
  deriving Lean.ToJson

structure PageObservation where
  offset : Nat
  paths : List String
  next : Option Nat
  deriving Lean.ToJson

structure SourceProjectionOutput where
  refusal : Option RefusalReason
  coverage : Option Coverage
  pages : List PageObservation
  deriving Lean.ToJson

/-- Native projection checks refine this subset of the executable source cases:
the served node publishes one tool with its own captures and projections.
Canonical admission, fork selection and transport remain separate consumers. -/
def sourceProjectionCases : List SelectionCase :=
  (selectionCases.filter fun value =>
    ["verified_unchanged_terminal", "unchanged_claim_cannot_carry_images",
     "unchanged_claim_cannot_retain_image_identity", "unchanged_claim_cannot_precede_terminal",
     "accepted_terminal_capture", "legacy_missing_capture_is_unknown",
     "active_missing_capture_is_pending", "uncommitted_capture_before_terminal",
     "projection_wrong_request", "projection_in_another_session_is_not_this_inventory",
     "projection_wrong_actual_requester", "projection_wrong_actual_node",
     "projection_foreign_stored_owner", "invalid_empty_capture_not_empty_inventory",
     "duplicate_paths_in_capture"].contains value.name) ++
  [⟨"one_capture_exceeds_one_page", oneLargeCapture, .selected 65 0 0 false⟩]

example : (sourceProjectionCases.map (·.name)).Nodup := by native_decide

def sourceProjectionSourceOf (name : String) : SourceInput :=
  ((sourceProjectionCases.find? (·.name == name)).map (·.source)).getD (source 1)

/-- Every page of the selected snapshot, followed from the first. -/
def pageObservations (current : Snapshot) : List PageObservation :=
  (List.range (max 1 ((current.entries.length + pageLimit - 1) / pageLimit))).map fun index =>
    let page := makePage current (index * pageLimit)
    { offset := page.offset, paths := page.entries.map (·.change.path)
      next := page.next.map (·.offset) }

/-- The wire carries each case to the native adapter; the model evaluates the
case its unique name selects. -/
@[conformance_family]
def sourceProjectionPages : Family SelectionInput SourceProjectionOutput where
  inputs := sourceProjectionCases.map fun value =>
    { name := value.name, source := .ofModel value.source, admission }
  model := fun i =>
    match snapshot (sourceProjectionSourceOf i.name).model i.admission with
    | .error error => { refusal := some error.refusal, coverage := none, pages := [] }
    | .ok current =>
        { refusal := none, coverage := some current.coverage, pages := pageObservations current }

structure SharedReaderInput where
  name : String
  source : SourceWire
  deriving Lean.ToJson

structure SharedReaderOutput where
  refusal : Option RefusalReason
  coverage : Option Coverage
  deriving Lean.ToJson

/-- One served-node fixture walks a shared reader through a missing capture, a
retained capture whose reader relation it toggles, a changed physical root and
an empty root; these are the reader cases that walk builds. -/
def sharedReaderCases : List SelectionCase :=
  FileInventory.ReaderExamples.cases.filter fun value =>
    ["shared_native_root_read", "shared_authorized_empty_root",
     "shared_root_physical_identity_changed", "shared_missing_capture_is_not_empty_complete",
     "shared_unreadable_projection_owner_is_not_trusted"].contains value.name

example : (sharedReaderCases.map (·.name)).Nodup := by native_decide

def sharedReaderSourceOf (name : String) : SourceInput :=
  ((sharedReaderCases.find? (·.name == name)).map (·.source)).getD FileInventory.ReaderExamples.shared

/-- The wire carries each case to the native adapter; the model evaluates the
case its unique name selects as the admission's authenticated reader. -/
@[conformance_family]
def sharedReaderSnapshot : Family SharedReaderInput SharedReaderOutput where
  inputs := sharedReaderCases.map fun value => { name := value.name, source := .ofModel value.source }
  model := fun i =>
    match directSnapshot (sharedReaderSourceOf i.name).model admission.requester admission with
    | .error error => { refusal := some error.refusal, coverage := none }
    | .ok current => { refusal := none, coverage := some current.coverage }

end Conformance.FileInventoryContracts
