import Proofs.SessionHydration.FileImages
import Proofs.SessionHydration.Receiver
import Proofs.ToolExecution.Executable

namespace SessionHydration.FileInventory

open CanonicalOutput (DocId)
open ToolExecution

inductive ImageFailure where
  | readFailed | changedDuringRead | tooLarge | unsupportedEntry | mutationIncomplete
  deriving DecidableEq, Repr

inductive CapturedImage where
  | missing
  | file (contentHash data : String)
  | unavailable (reason : ImageFailure)
  deriving DecidableEq, Repr

/-- Metadata contains no image bytes. Content hashes identify retained images;
they do not grant access to them or establish an edit's position in a chain. -/
inductive ImageMetadata where
  | missing
  | file (contentHash : String)
  | unavailable (reason : ImageFailure)
  deriving DecidableEq, Repr

def CapturedImage.metadata : CapturedImage → ImageMetadata
  | .missing => .missing
  | .file hash _ => .file hash
  | .unavailable reason => .unavailable reason

theorem file_metadata_erases_bytes (hash left right : String) :
    (CapturedImage.file hash left).metadata = (CapturedImage.file hash right).metadata := rfl

structure CapturedChange where
  root : String
  path : String
  before : CapturedImage
  after : CapturedImage
  deriving DecidableEq, Repr

structure Change where
  root : String
  path : String
  before : ImageMetadata
  after : ImageMetadata
  deriving DecidableEq, Repr

def CapturedChange.metadata (change : CapturedChange) : Change :=
  ⟨change.root, change.path, change.before.metadata, change.after.metadata⟩

structure Observation where
  id : DocId
  tool : DocId
  request : DocId
  scope : FileImages.Scope
  changes : List CapturedChange
  deriving DecidableEq, Repr

/-- This is a distinct immutable metadata projection. Storing it inside the
byte-bearing observation would still replicate that observation's image bytes.
The native capture decoder owns path, hash, permission and byte validation. -/
structure Projection where
  id : DocId
  observation : DocId
  tool : DocId
  request : DocId
  scope : FileImages.Scope
  /-- DefraDB's authenticated stored creator, not an authored JSON claim. -/
  creator : String
  changes : List Change
  deriving DecidableEq, Repr

def Observation.metadata (observation : Observation) (projectionId : DocId) : Projection :=
  ⟨projectionId, observation.id, observation.tool, observation.request,
    observation.scope, observation.scope.node, observation.changes.map CapturedChange.metadata⟩

/-- `won` is the existing terminal CAS result, and `write` is that transaction's
commit result. Publication shares the winning terminal/capture transaction;
failure, cancellation and timeout do not erase a captured mutation. -/
def publish (pre : ToolCallContext) (action : ToolCallContext.Action)
    (won : Bool) (write : TerminalWriteResult) (observation : Observation)
    (projectionId : DocId) : Option Projection := do
  let post ← pre.step? action
  if won && write == .committed && decide (HasTerminal.isTerminal post.state) &&
      observation.tool == post.callId && observation.request == post.requestId &&
      !observation.changes.isEmpty then
    some (observation.metadata projectionId)
  else none

theorem losing_terminal_publishes_nothing (pre : ToolCallContext)
    (action : ToolCallContext.Action) (write : TerminalWriteResult)
    (observation : Observation) (projectionId : DocId) :
    publish pre action false write observation projectionId = none := by
  simp [publish]

theorem rolled_back_terminal_publishes_nothing (pre : ToolCallContext)
    (action : ToolCallContext.Action) (won : Bool)
    (observation : Observation) (projectionId : DocId) :
    publish pre action won .failed observation projectionId = none := by
  simp [publish]

/-- `accepted` is supplied by the existing physical tool acceptance owner,
including its coordinator block and direct/spawn/plugin provenance checks.
The actual scope is retained for origins; it is not rewritten to the fork's
target session. Canonical closure authorization decides origin membership. -/
structure Tool where
  id : DocId
  request : DocId
  scope : FileImages.Scope
  accepted : Bool
  state : ToolCallState
  deriving DecidableEq, Repr

inductive Capture where
  | pending
  | unknown
  | observed (projection : Projection)
  deriving DecidableEq, Repr

structure ToolFact where
  tool : Tool
  capture : Capture
  deriving DecidableEq, Repr

/-- Equality names all canonical membership and mutable terminal/capture facts,
not a sequence watermark or wall-clock ordering. A native encoding must bind
this value in a new signed metadata purpose; a v1 transcript manifest does not
bind mutable tool revisions. Digest collision resistance and source signature
verification remain the existing native cryptographic premises. -/
structure Snapshot where
  admission : Request
  documents : List CanonicalOutput.Hydration.DocumentKey
  tools : List ToolFact
  deriving DecidableEq, Repr

inductive ReadBoundary where
  | atomicExhaustive
  | incomplete
  | nonAtomic
  deriving DecidableEq, Repr

/-- `atomicExhaustive` is an external premise of the ConfigAccess read owner:
all canonical membership, accepted bindings, terminal facts and projections
were enumerated in one consistent database read snapshot with bounded native
queries. The current multi-read hydration loader does not establish it merely
by returning a list. A truncated or mixed-revision read cannot certify empty. -/
structure Source where
  catalog : Catalog
  tools : List Tool
  projections : List Projection
  boundary : ReadBoundary
  deriving DecidableEq

inductive Error where
  | rejected
  | unknown
  deriving DecidableEq, Repr

def validScope (scope : FileImages.Scope) : Bool :=
  [scope.requester, scope.node, scope.session].all (fun value => !value.isEmpty)

def Projection.matches (projection : Projection) (tool : Tool) : Bool :=
  projection.tool == tool.id && projection.request == tool.request &&
    projection.scope == tool.scope && projection.creator == tool.scope.node &&
    !projection.changes.isEmpty &&
    projection.changes.all (fun change => !change.root.isEmpty && !change.path.isEmpty) &&
    (projection.changes.map (fun change => (change.root, change.path))).Nodup

def toolFact (source : Source) (id : DocId) : Except Error ToolFact := do
  let tool ← match source.tools.filter (fun tool => tool.id == id) with
    | [] => .error .unknown
    | [tool] => .ok tool
    | _ => .error .rejected
  if !tool.accepted || !validScope tool.scope then .error .rejected else pure ()
  let projections := source.projections.filter (fun projection => projection.tool == id)
  if !decide (HasTerminal.isTerminal tool.state) then
    if projections.isEmpty then .ok ⟨tool, .pending⟩ else .error .rejected
  else match projections with
    | [] => .ok ⟨tool, .unknown⟩
    | [projection] =>
      if projection.matches tool then .ok ⟨tool, .observed projection⟩ else .error .rejected
    | _ => .error .rejected

def selectedManifest (catalog : Catalog) (admission : Request) :
    Except Error (List CanonicalOutput.Hydration.DocumentKey) := do
  let input ← match closureInputFor catalog.closureInputs admission with
    | none => .error .unknown
    | some input => .ok input
  match CanonicalOutput.Hydration.buildManifest input.request.authorizationScope
    input.request.nativeSession input.bases input.roots input.requirements
    input.messages input.segments input.access input.deniedHeaders input.deniedSegments
    input.dependencyDenials input.effects with
  | .ok documents => .ok documents
  | .error error => .error (if hydrationErrorIsLoading error then .unknown else .rejected)

theorem selected_manifest_uses_canonical_closure (catalog : Catalog) (admission : Request)
    (documents : List CanonicalOutput.Hydration.DocumentKey)
    (h : selectedManifest catalog admission = .ok documents) :
    selectedDocuments catalog admission = some (documents.map canonicalDocument).toFinset := by
  cases hi : closureInputFor catalog.closureInputs admission with
  | none => simp [selectedManifest, hi, Bind.bind, Except.bind] at h
  | some input =>
    cases hb : CanonicalOutput.Hydration.buildManifest input.request.authorizationScope
        input.request.nativeSession input.bases input.roots input.requirements input.messages
        input.segments input.access input.deniedHeaders input.deniedSegments
        input.dependencyDenials input.effects with
    | error error => simp [selectedManifest, hi, hb, Bind.bind, Except.bind] at h
    | ok manifest =>
      have equal : manifest = documents := by
        simpa [selectedManifest, hi, hb, Bind.bind, Except.bind] using h
      subst documents
      simp [selectedDocuments, hi, buildDocuments, hb, Bind.bind, Except.bind,
        Except.toOption, pure, Except.pure]

/-- Enumeration starts from the whole authorized session closure, not a
visible transcript window or request-turn closure. Unselected parent-session
tools do not enter a fork's inventory. Physical sorting is only a stable page
order; the existing before/after chain planner still owns review authority. -/
def snapshot (source : Source) (admission : Request) : Except Error Snapshot := do
  if !decide (admits source.catalog admission) then .error .rejected else pure ()
  if source.boundary != .atomicExhaustive then .error .unknown else pure ()
  let selected ← selectedManifest source.catalog admission
  let toolIds := (selected.filter (fun key => key.collection == .agentToolCall)).map (·.id)
  let tools ← toolIds.mapM (toolFact source)
  let observed := tools.filterMap fun fact => match fact.capture with
    | .observed projection => some projection
    | _ => none
  if !(observed.map (·.id)).Nodup || !(observed.map (·.observation)).Nodup then
    .error .rejected
  else .ok ⟨admission, selected, tools⟩

structure Entry where
  tool : DocId
  request : DocId
  scope : FileImages.Scope
  projection : DocId
  observation : DocId
  change : Change
  deriving DecidableEq, Repr

def Snapshot.entries (value : Snapshot) : List Entry :=
  value.tools.flatMap fun fact => match fact.capture with
    | .observed projection => projection.changes.map fun change =>
        ⟨fact.tool.id, fact.tool.request, fact.tool.scope,
          projection.id, projection.observation, change⟩
    | _ => []

def Snapshot.pending (value : Snapshot) : List DocId :=
  (value.tools.filter (fun fact => fact.capture == .pending)).map (·.tool.id)

def Snapshot.unknown (value : Snapshot) : List DocId :=
  (value.tools.filter (fun fact => fact.capture == .unknown)).map (·.tool.id)

/-- A missing capture is unknown coverage, never evidence of an unchanged
filesystem. Even a known empty inventory makes no claim about shell, plugin or
external writes which the native capture owner did not observe. -/
def Snapshot.knownEmpty (value : Snapshot) : Bool :=
  value.entries.isEmpty && value.pending.isEmpty && value.unknown.isEmpty

def pageLimit : Nat := 64

structure Cursor where
  snapshot : Snapshot
  offset : Nat
  deriving DecidableEq, Repr

structure Page where
  snapshot : Snapshot
  offset : Nat
  entries : List Entry
  next : Option Cursor
  deriving DecidableEq, Repr

/-- Snapshot values are logical signed preimages, not a requirement to repeat
the full membership and projection list on every wire page. The native typed
purpose must bind their canonical digest, exact scope and page position. -/
structure Receiver where
  snapshot : Snapshot
  entries : List Entry
  complete : Bool
  deriving DecidableEq, Repr

inductive PageResult where
  | rejected
  | unknown
  | stale
  | page (value : Page)
  deriving DecidableEq, Repr

/-- Only a verified source signature for the explicit metadata purpose can
establish this value. Transcript v1 receipts are intentionally a disjoint
constructor and cannot prove a metadata snapshot, even for the same IDs. -/
inductive VerifiedReceipt where
  | transcriptV1 (admission : Request) (documents : List CanonicalOutput.Hydration.DocumentKey)
  | metadata (snapshot : Snapshot)
  deriving DecidableEq, Repr

def VerifiedReceipt.proves (receipt : VerifiedReceipt) (value : Snapshot) : Bool :=
  match receipt with
  | .transcriptV1 _ _ => false
  | .metadata expected => expected == value

def makePage (value : Snapshot) (offset : Nat) : Page :=
  let entries := (value.entries.drop offset).take pageLimit
  let following := offset + entries.length
  ⟨value, offset, entries,
    if following < value.entries.length then some ⟨value, following⟩ else none⟩

def page (source : Source) (admission : Request) (receipt : VerifiedReceipt)
    (cursor : Option Cursor) : PageResult :=
  match snapshot source admission with
  | .error .rejected => .rejected
  | .error .unknown => .unknown
  | .ok current =>
      if !receipt.proves current then .stale
      else match cursor with
      | none => .page (makePage current 0)
      | some cursor =>
          if cursor.snapshot != current then .stale
          else if cursor.offset == 0 || cursor.offset % pageLimit != 0 ||
              cursor.offset >= current.entries.length then .rejected
          else .page (makePage current cursor.offset)

theorem page_is_bounded (value : Snapshot) (offset : Nat) :
    (makePage value offset).entries.length ≤ pageLimit := by
  simp only [makePage]
  exact List.length_take_le _ _

theorem transcript_receipt_cannot_prove_metadata
    (admission : Request) (documents : List CanonicalOutput.Hydration.DocumentKey) (value : Snapshot) :
    (VerifiedReceipt.transcriptV1 admission documents).proves value = false := rfl

theorem changed_snapshot_invalidates_old_receipt (before after : Snapshot)
    (h : before ≠ after) :
    (VerifiedReceipt.metadata before).proves after = false := by
  simp [VerifiedReceipt.proves, h]

def beginReceive (receipt : VerifiedReceipt) : Option Receiver :=
  match receipt with
  | .transcriptV1 _ _ => none
  | .metadata value => some ⟨value, [], false⟩

/-- A signed later page cannot certify an omitted prefix. End-of-pages and
known capture coverage remain distinct: legacy missing capture stays unknown
even after every page has arrived. -/
def receive (state : Receiver) (value : Page) : Option Receiver :=
  if state.complete || value.snapshot != state.snapshot ||
      state.entries != state.snapshot.entries.take state.entries.length ||
      value.offset != state.entries.length ||
      value != makePage state.snapshot state.entries.length then none
  else some ⟨state.snapshot, state.entries ++ value.entries, value.next.isNone⟩

def knownInventory (state : Receiver) : Bool :=
  state.complete && state.snapshot.pending.isEmpty && state.snapshot.unknown.isEmpty

end SessionHydration.FileInventory
