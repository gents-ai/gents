import Mathlib.Data.String.Basic
import Proofs.SessionHydration.FileImages
import Proofs.SessionHydration.Receiver
import Proofs.ToolExecution.Executable
import Proofs.CanonicalOutput.ToolDelivery

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
  id : Option DocId
  tool : DocId
  request : DocId
  scope : FileImages.Scope
  changes : List CapturedChange
  /-- Only the native producer of a read-only operation can supply this witness.
  It is not inferred from success, a name, empty output or missing capture. -/
  verifiedUnchanged : Bool := false
  deriving DecidableEq, Repr

/-- This is a distinct immutable metadata projection. Storing it inside the
byte-bearing observation would still replicate that observation's image bytes.
The native capture decoder owns path, hash, permission and byte validation.
Physical IDs are the existing model's abstract names for successfully decoded
native documents, never a way to accept empty or fabricated native `_docID`s.
The current owner comes from the transaction-bound ACP observation, not the
stored metadata JSON or its authored node/signer columns. -/
structure Projection where
  id : DocId
  observation : Option DocId
  tool : DocId
  request : DocId
  scope : FileImages.Scope
  /-- DefraDB's current authenticated document owner, not an authored JSON claim. -/
  owner : String
  changes : List Change
  verifiedUnchanged : Bool := false
  deriving DecidableEq, Repr

def Observation.metadata (observation : Observation) (projectionId : DocId)
    (agentDid : String) : Projection :=
  ⟨projectionId, observation.id, observation.tool, observation.request,
    observation.scope, agentDid, observation.changes.map CapturedChange.metadata,
    observation.verifiedUnchanged⟩

def Observation.wellFormed (observation : Observation) : Bool :=
  if observation.verifiedUnchanged then observation.id.isNone && observation.changes.isEmpty
  else observation.id.isSome && !observation.changes.isEmpty

/-- `accepted` is supplied by the existing physical tool acceptance owner,
including its coordinator block and direct/spawn/plugin provenance checks.
A fork does not copy physical tools: the actual scope is retained for origins
and is not rewritten to the fork's target session. Canonical closure
authorization decides origin membership; the fork's own projection copies,
not the origin's projections, supply its inventory facts. -/
structure Tool where
  id : DocId
  request : DocId
  scope : FileImages.Scope
  /-- The selected Agent DID comes from the canonical source session/accepted tool,
  not authored capture metadata. It owns retained captures and projections. The
  existing native reader-relation mechanism may grant the canonical session user
  access under this agent actor; no runtime-key fallback is permitted. Generic
  configured-user inheritance remains the #3485 policy obligation. -/
  agentDid : String
  accepted : Bool
  state : ToolCallState
  deriving DecidableEq, Repr

/-- `won` is the existing terminal CAS result, and `write` is that transaction's
commit result. Publication shares the winning terminal/capture transaction;
failure, cancellation and timeout do not erase a captured mutation. `actual`
is the accepted physical row which owns that CAS, not the inventory target's
scope or a scope supplied in authored capture data. -/
def publish (pre : ToolCallContext) (action : ToolCallContext.Action)
    (won : Bool) (write : TerminalWriteResult) (actual : Tool) (observation : Observation)
    (projectionId : DocId) : Option Projection := do
  let post ← pre.step? action
  if won && write == .committed && decide (HasTerminal.isTerminal post.state) &&
      !actual.agentDid.isEmpty && actual.accepted && actual.id == pre.callId && actual.request == pre.requestId &&
      actual.state == pre.state && observation.scope == actual.scope &&
      observation.tool == post.callId && observation.request == post.requestId &&
      observation.wellFormed then
    some (observation.metadata projectionId actual.agentDid)
  else none

theorem losing_terminal_publishes_nothing (pre : ToolCallContext)
    (action : ToolCallContext.Action) (write : TerminalWriteResult)
    (actual : Tool) (observation : Observation) (projectionId : DocId) :
    publish pre action false write actual observation projectionId = none := by
  simp [publish]

theorem rolled_back_terminal_publishes_nothing (pre : ToolCallContext)
    (action : ToolCallContext.Action) (won : Bool)
    (actual : Tool) (observation : Observation) (projectionId : DocId) :
    publish pre action won .failed actual observation projectionId = none := by
  simp [publish]

theorem different_capture_scope_publishes_nothing (pre : ToolCallContext)
    (action : ToolCallContext.Action) (won : Bool) (write : TerminalWriteResult)
    (actual : Tool) (observation : Observation) (projectionId : DocId)
    (h : observation.scope ≠ actual.scope) :
    publish pre action won write actual observation projectionId = none := by
  simp [publish, h]

inductive Capture where
  | pending
  | unknown
  | observed (projection : Projection)
  deriving DecidableEq, Repr

structure ToolFact where
  tool : Tool
  capture : Capture
  deriving DecidableEq, Repr

namespace Order

/-- These are canonical publication positions, not tool IDs, block positions,
wall-clock observations or a new mutation journal. A completed publication
before the next admission establishes an edge even when requests differ.
Overlapping intervals do not establish an order between their mutations. -/
structure Window where
  tool : DocId
  admitted : Nat
  delivered : Nat
  deriving DecidableEq, Repr

def separated : List Window → Bool
  | [] | [_] => true
  | left :: right :: rest =>
      decide (left.delivered < right.admitted) && separated (right :: rest)

/-- This selects chronology only. Image continuity and inventory coverage must
still be established by their existing owners before producing a net diff. -/
def resolve (windows : List Window) : Option (List DocId) :=
  let ordered := windows.mergeSort (fun left right => left.admitted ≤ right.admitted)
  if (windows.map (·.tool)).Nodup &&
      windows.all (fun window => window.admitted < window.delivered) && separated ordered then
    some (ordered.map (·.tool))
  else none

theorem overlapping_adjacent_windows_are_unresolved (left right : Window)
    (rest : List Window) (h : right.admitted ≤ left.delivered) :
    separated (left :: right :: rest) = false := by
  simp [separated, Nat.not_lt.mpr h]

theorem empty_order : resolve [] = some [] := by simp [resolve, separated]

inductive Position where
  | unresolved
  | ordered (index entries : Nat)
  deriving DecidableEq, Repr

def Position.wellFormed : Position → Bool
  | .unresolved => true
  | .ordered index entries => index < entries

def Position.within (total : Nat) : Position → Bool
  | .unresolved => true
  | .ordered _ entries => entries ≤ total

end Order

/-- Equality names canonical membership, mutable terminal/capture facts and
their selected ordering observations, not a sequence watermark or wall clock.
A native encoding must bind
this value in a new signed metadata purpose; a v1 transcript manifest does not
bind mutable tool revisions. Digest collision resistance and source signature
verification remain the existing native cryptographic premises. -/
structure Snapshot where
  admission : Request
  documents : List CanonicalOutput.Hydration.DocumentKey
  tools : List ToolFact
  ordering : List (Option Order.Window) := []
  deriving DecidableEq, Repr

theorem different_ordering_changes_snapshot (before after : Snapshot)
    (h : before.ordering ≠ after.ordering) : before ≠ after := by
  intro equal
  exact h (congrArg Snapshot.ordering equal)

namespace Order

open CanonicalOutput

private def origin (input : ClosureInput) : Nat → MessageEnvelope → Option MessageEnvelope
  | 0, _ => none
  | fuel + 1, message =>
      match Hydration.checkedOrigin message.header with
      | .error _ => none
      | .ok none => some message
      | .ok (some id) =>
          match Hydration.lookupMessage input.messages input.deniedHeaders id with
          | .error _ => none
          | .ok original =>
              if Hydration.forkMetadataMatches message original then origin input fuel original
              else none

private def calls (tool : Tool) (message : MessageEnvelope) : Bool :=
  message.header.request == some tool.request && message.header.role == .assistant &&
    (match message.header.publication with | .requestExecution _ => true | _ => false) &&
    message.blocks.any (fun block => match block with
      | .toolCall id _ _ _ _ _ _ => id == tool.id
      | _ => false)

private def terminalReference (input : ClosureInput) (tool : Tool)
    (reference : PayloadRef) : Bool :=
  match resolveClose input.segments input.deniedSegments reference with
  | .error _ => false
  | .ok closing => CanonicalOutput.ToolDelivery.closedRecordValid
      input.segments tool.request tool.id closing

/-- A background acknowledgement uses an authored receipt source, even when its
ToolResult shape matches a completed result. Only the existing owned tool-output
closure establishes that the mutation finished before this publication.
A text notification may name a different wake request; its retained reference
still binds the original physical tool/request, without interpreting its text. -/
private def delivers (input : ClosureInput) (tool : Tool) (message : MessageEnvelope) : Bool :=
  message.header.role == .user && message.header.request.isSome &&
    message.header.publication == .toolDelivery tool.id &&
    (match message.blocks with
      | [.toolResult id _ _ _] => id == tool.id && message.header.request == some tool.request
      | blocks => !blocks.isEmpty && blocks.all (fun block =>
          match block with | .text _ => true | _ => false)) &&
    (message.blocks.flatMap blockRefs).any (terminalReference input tool)

/-- A copied fork header retains its origin's role, sequence and blocks but not
its request or publication kind, so the admission is the assistant header that
invokes this physical tool. The fork's creating transaction copied it from the
validated origin. -/
private def copyCalls (tool : Tool) (message : MessageEnvelope) : Bool :=
  message.header.role == .assistant &&
    message.blocks.any (fun block => match block with
      | .toolCall id _ _ _ _ _ _ => id == tool.id
      | _ => false)

/-- A copied delivery is a sole ToolResult for this tool whose payload resolves,
in the fork's own copies, to that tool's owned output closure. A copied text
notification loses the publication that bound it to the tool and stays
unresolved. -/
private def copyDelivers (copies : List Segment) (tool : Tool) (message : MessageEnvelope) : Bool :=
  let records := copies.map Hydration.presentCopy
  message.header.role == .user &&
    (match message.blocks with
      | [.toolResult id _ _ _] => id == tool.id
      | _ => false) &&
    (message.blocks.flatMap blockRefs).any fun reference =>
      match resolveClose records [] reference with
      | .error _ => false
      | .ok closing => CanonicalOutput.ToolDelivery.closedRecordValid
          records closing.coordinate.request tool.id closing

/-- A fork header with destination-owned copies is read from those copies,
refining `Hydration.forkCopies`; a fork published before copies follows its
origin. -/
private def uniquePosition (input : ClosureInput) (messages : List MessageEnvelope)
    (predicate : MessageEnvelope → Bool) (copied : List Segment → MessageEnvelope → Bool) :
    Option Nat :=
  match messages.filter (fun message =>
    match Hydration.forkCopies message.header input.segments with
    | some copies => copied copies message
    | none => (origin input (input.messages.length + 1) message).any predicate) with
  | [message] => some message.sequence
  | _ => none

/-- The snapshot and closure input are outputs of the existing authorized
inventory/hydration owners. Only selected target-session headers contribute
positions. Following a fork's origin validates that selected header; it never
adds an origin's later publications beyond the inherited cut. A missing or
ambiguous admission/delivery remains unresolved, including tools whose native
provenance does not supply a direct invocation-result pair. -/
def window (snapshot : Snapshot) (input : ClosureInput) (tool : Tool) : Option Window := do
  if input.request != snapshot.admission || !tool.accepted ||
      !(snapshot.tools.any (fun fact => fact.tool == tool)) ||
      !decide (HasTerminal.isTerminal tool.state) then none else pure ()
  let ids := (snapshot.documents.filter (fun key => key.collection == .agentMessage)).map (·.id)
  let messages ← ids.mapM fun id => (Hydration.lookupMessage input.messages input.deniedHeaders id).toOption
  let selected := messages.filter (fun message =>
    message.header.session == snapshot.admission.nativeSession)
  let admitted ← uniquePosition input selected (calls tool) (fun _ => copyCalls tool)
  let delivered ← uniquePosition input selected (delivers input tool) (copyDelivers · tool)
  if admitted < delivered then some ⟨tool.id, admitted, delivered⟩ else none

end Order

inductive ReadBoundary where
  | atomicExhaustive
  | readerBound (scope : CanonicalOutput.Hydration.AuthorizationScope)
  | incomplete
  | nonAtomic
  deriving DecidableEq, Repr

/-- `atomicExhaustive` is an external premise of the ConfigAccess read owner:
all canonical membership, accepted bindings, terminal facts, projections and
current document-owner observations were enumerated in one consistent database
read snapshot with bounded native queries. The current multi-read hydration loader does not establish it merely
by returning a list. A truncated or mixed-revision read cannot certify empty. -/
structure Source where
  catalog : Catalog
  tools : List Tool
  projections : List Projection
  boundary : ReadBoundary
  /-- DefraDB's current authenticated owner of the admission's physical session
  document, observed in the same read as the projections. It is not an authored
  session field; empty means unobserved. A fork's creating actor owns the fork
  session and the projection copies it published in the same transaction. -/
  sessionOwner : String := ""
  deriving DecidableEq

/-- An independent reader cannot reuse the source's privileged observations.
`readerBound` certifies that every supplied canonical/metadata read observation
belongs to that reader in the same exhaustive source snapshot. A missing or
denied capture cannot be supplied as an authenticated projection. No image is
read or granted by this boundary. -/
def Source.completeRead (source : Source) (admission : Request) : Bool :=
  match source.boundary with
  | .atomicExhaustive => decide (ownedSession admission ∈ source.catalog.sessions)
  | .readerBound scope => scope == admission.authorizationScope
  | .incomplete | .nonAtomic => false

inductive Error where
  | rejected
  | unknown
  deriving DecidableEq, Repr

def validScope (scope : FileImages.Scope) : Bool :=
  [scope.requester, scope.node, scope.session].all (fun value => !value.isEmpty)

/-- A published fork is self-contained (user decision): it carries copies of
the file-inventory projections of the selected tools it inherits, published in
the fork's creating transaction, scoped to the fork session and owned by the
fork's destination owner. It never reads its source session's projections, so
a copy is retained when the source's projections or session are deleted, and
the desktop keeps listing the files a forked conversation touched. Physical
tool, request and observation IDs remain identities, not session references.

A session's own native tool keeps its producing Agent as projection owner.
An inherited tool's fact is the copy in the admission's session, with the
origin's node and requester (forks never cross them), owned by the observed
`sessionOwner`. Copy contents are the fork writer's obligation: it copies the
exact source-scope projection its signed actor can read in that transaction,
and rejects one whose observed owner is neither the source's Agent nor the
source session's owner. DefraDB's HTTP owner observation only reports whether
the caller owns a document, so an Agent's ownership is unobservable to a
client fork; the destination owner is the trust root for its own copies, and
the reader cannot re-check a deleted source. A copy is metadata only and grants no image read:
retained images stay bound to the producing tool's scope. -/
def Projection.matches (projection : Projection) (tool : Tool) (session sessionOwner : String) :
    Bool :=
  projection.tool == tool.id && projection.request == tool.request &&
    projection.scope == { tool.scope with session := session } && !tool.agentDid.isEmpty &&
    (if tool.scope.session == session then projection.owner == tool.agentDid
     else !sessionOwner.isEmpty && projection.owner == sessionOwner) &&
    (if projection.verifiedUnchanged then
      projection.observation.isNone && projection.changes.isEmpty
     else projection.observation.isSome && !projection.changes.isEmpty) &&
    projection.changes.all (fun change => !change.root.isEmpty && !change.path.isEmpty) &&
    (projection.changes.map (fun change => (change.root, change.path))).Nodup

/-- Only projections in the admission's own session are its facts. Another
session's projection of the same tool, such as a fork's copy or the origin of
an inherited tool, belongs to that session's inventory and is neither evidence
nor a conflict here. -/
def toolFact (source : Source) (session : String) (id : DocId) : Except Error ToolFact := do
  let tool ← match source.tools.filter (fun tool => tool.id == id) with
    | [] => .error .unknown
    | [tool] => .ok tool
    | _ => .error .rejected
  if !tool.accepted || !validScope tool.scope then .error .rejected else pure ()
  let projections := source.projections.filter (fun projection =>
    projection.tool == id && projection.scope.session == session)
  if !decide (HasTerminal.isTerminal tool.state) then
    if projections.isEmpty then .ok ⟨tool, .pending⟩ else .error .rejected
  else match projections with
    | [] => .ok ⟨tool, .unknown⟩
    | [projection] =>
      if projection.matches tool session source.sessionOwner then .ok ⟨tool, .observed projection⟩ else .error .rejected
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

/-- The tool facts a fork copy carries. A copy exists only for a terminal
accepted capture, so its terminal kind is not inventory data; its scope is the
fork's and its owner the fork's creator. -/
def Projection.copyTool (projection : Projection) : Tool :=
  ⟨projection.tool, projection.request, projection.scope, projection.owner, true, .completed⟩

/-- Physical tools a selected header names, as hydration selects them. -/
def messageTools (message : CanonicalOutput.MessageEnvelope) : List DocId :=
  (match message.header.publication with
    | .toolDelivery id => [id]
    | _ => []) ++
  message.blocks.filterMap fun block => match block with
    | .toolCall id _ _ _ _ _ _ => some id
    | .toolResult id _ _ _ => some id
    | _ => none

/-- A self-contained fork's inherited facts: projections in the admission's
session, owned by its observed `sessionOwner`, for tools named by its selected
headers but absent from its canonical closure. A projection of an unselected
tool owned by anyone else is unselected work, not this inventory. -/
def copiedFacts (source : Source) (admission : Request) (input : ClosureInput)
    (selected : List CanonicalOutput.Hydration.DocumentKey) (native : List DocId) :
    Except Error (List ToolFact) := do
  if source.sessionOwner.isEmpty then return []
  let headers := (selected.filter (fun key => key.collection == .agentMessage)).filterMap fun key =>
    (CanonicalOutput.Hydration.lookupMessage input.messages input.deniedHeaders key.id).toOption
  let named := (headers.filter (·.header.session == admission.nativeSession)).flatMap messageTools
  let copies := source.projections.filter fun projection =>
    projection.scope.session == admission.session && projection.owner == source.sessionOwner &&
      !native.contains projection.tool && named.contains projection.tool
  copies.mapM fun projection =>
    let owners := source.catalog.sessions.filter fun session =>
      session.node == admission.node && session.session == admission.session &&
        session.requester == projection.scope.requester
    let tool := projection.copyTool
    if (copies.filter (·.tool == projection.tool)).length == 1 && validScope projection.scope &&
        projection.scope.node == admission.node &&
        owners.card == 1 &&
        projection.matches tool admission.session source.sessionOwner then
      .ok ⟨tool, .observed projection⟩
    else .error .rejected

/-- Enumeration starts from the whole authorized session closure, not a
visible transcript window or request-turn closure. Unselected parent-session
tools do not enter a fork's inventory. Physical sorting is only a stable page
order; the existing before/after chain planner still owns review authority. -/
private def assembleSnapshot (source : Source) (admission : Request) : Except Error Snapshot := do
  if !source.completeRead admission then .error .unknown else pure ()
  let selected ← selectedManifest source.catalog admission
  let toolIds := (selected.filter (fun key => key.collection == .agentToolCall)).map (·.id)
  let native ← toolIds.mapM (toolFact source admission.session)
  let input ← match closureInputFor source.catalog.closureInputs admission with
    | none => .error .unknown
    | some input => .ok input
  let copied ← copiedFacts source admission input selected toolIds
  let tools := (native ++ copied).mergeSort fun left right => left.tool.id ≤ right.tool.id
  let observed := tools.filterMap fun fact => match fact.capture with
    | .observed projection => some projection
    | _ => none
  if !(observed.map (·.id)).Nodup || !(observed.filterMap (·.observation)).Nodup then
    .error .rejected
  else
    let initial : Snapshot := ⟨admission, selected, tools, []⟩
    .ok { initial with ordering := tools.map fun fact => Order.window initial input fact.tool }

def snapshot (source : Source) (admission : Request) : Except Error Snapshot := do
  if !decide (admits source.catalog admission) then .error .rejected else pure ()
  assembleSnapshot source admission

/-- Direct inventory reads use the authenticated ConfigAccess actor and native
reader-bound observations from one database transaction. Session requester fields
remain creation provenance; they need not equal the reader. There is no enrollment,
node-owner preflight, runtime signature, transfer journal, or filesystem scan.
Every selected capture still needs native visibility and its producing Agent owner. -/
def directSnapshot (source : Source) (actor : String) (admission : Request) :
    Except Error Snapshot := do
  if actor.isEmpty || actor != admission.requester then .error .rejected else pure ()
  if source.boundary != .readerBound admission.authorizationScope then .error .unknown else pure ()
  let sessions := source.catalog.sessions.filter (fun session =>
    session.node == admission.node && session.session == admission.session)
  if sessions.card != 1 then .error .unknown else pure ()
  if !(source.catalog.sessionReads.any fun observation =>
      decide (observation.source ∈ sessions) && observation.matches admission observation.source)
  then .error .rejected else pure ()
  assembleSnapshot source admission

theorem anonymous_direct_reader_is_denied (source : Source) (admission : Request) :
    directSnapshot source "" admission = .error .rejected := by
  simp [directSnapshot, String.isEmpty, Bind.bind, Except.bind]

structure Coverage where
  entries : Nat
  pending : Nat
  unknown : Nat
  deriving DecidableEq, Repr

/-- Coverage is derived from every selected physical tool in this exact source
scope and request. Absence of a retained capture remains unknown; other requests
cannot establish or invalidate completeness of this request. -/
def Snapshot.requestCoverage (value : Snapshot) (scope : FileImages.Scope)
    (request : DocId) : Coverage :=
  let facts := value.tools.filter fun fact =>
    fact.tool.scope == scope && fact.tool.request == request
  ⟨(facts.flatMap fun fact => match fact.capture with
      | .observed projection => projection.changes
      | _ => []).length,
    (facts.filter fun fact => fact.capture == .pending).length,
    (facts.filter fun fact => fact.capture == .unknown).length⟩

structure Entry where
  tool : DocId
  request : DocId
  /-- The scope of the selected projection: the admission's own session, also
  for a fork's inherited tool. -/
  scope : FileImages.Scope
  projection : DocId
  observation : DocId
  change : Change
  order : Order.Position := .unresolved
  requestCoverage : Option Coverage := none
  deriving DecidableEq, Repr

def Snapshot.rawEntries (value : Snapshot) : List Entry :=
  value.tools.flatMap fun fact => match fact.capture with
    | .observed projection => match projection.observation with
        | none => []
        | some observation => projection.changes.map fun change =>
            ⟨fact.tool.id, fact.tool.request, projection.scope,
              projection.id, observation, change, .unresolved,
              some (value.requestCoverage fact.tool.scope fact.tool.request)⟩
    | _ => []

def Snapshot.orderedTools (value : Snapshot) (node root path : String) : Option (List DocId) := do
  if [node, root, path].any String.isEmpty then none else pure ()
  let ids := ((value.rawEntries.filter (fun entry =>
    entry.scope.node == node && entry.change.root == root && entry.change.path == path)).map
      (·.tool)).dedup
  if ids.isEmpty then none else pure ()
  let windows ← ids.mapM fun toolId =>
    match (value.ordering.filterMap id).filter (fun observation => observation.tool == toolId) with
    | [observation] => some observation
    | _ => none
  Order.resolve windows

def Snapshot.entries (value : Snapshot) : List Entry :=
  value.rawEntries.map fun entry =>
    let order := match value.orderedTools entry.scope.node entry.change.root entry.change.path with
      | none => .unresolved
      | some ids => match ids.idxOf? entry.tool with
        | none => .unresolved
        | some index => .ordered index ids.length
    { entry with order }

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

inductive PageResult where
  | rejected
  | unknown
  | stale
  | page (value : Page)
  deriving DecidableEq, Repr

/-- Source-side logical commitment. The full value is not a receiver wire
field. Its native digest encoding and authenticated page rendering must bind
this exact preimage; transcript v1 has a disjoint purpose. -/
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

def pageFromSnapshot (current : Snapshot) (cursor : Option Cursor) : PageResult :=
  match cursor with
  | none => .page (makePage current 0)
  | some cursor =>
      if cursor.snapshot != current then .stale
      else if cursor.offset == 0 || cursor.offset % pageLimit != 0 ||
          cursor.offset >= current.entries.length then .rejected
      else .page (makePage current cursor.offset)

def page (source : Source) (admission : Request) (receipt : VerifiedReceipt)
    (cursor : Option Cursor) : PageResult :=
  match snapshot source admission with
  | .error .rejected => .rejected
  | .error .unknown => .unknown
  | .ok current =>
      if !receipt.proves current then .stale else pageFromSnapshot current cursor

def directPage (source : Source) (actor : String) (admission : Request)
    (cursor : Option Cursor) : PageResult :=
  match directSnapshot source actor admission with
  | .error .rejected => .rejected
  | .error .unknown => .unknown
  | .ok current => pageFromSnapshot current cursor

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

def Snapshot.coverage (value : Snapshot) : Coverage :=
  ⟨value.entries.length, value.pending.length, value.unknown.length⟩

/-- Digest strings are opaque native canonical commitments, not decoded
snapshots. Signature verification binds every page field and its digest to
the source; the receiver does not infer unreceived membership from a hash. -/
structure PageBinding where
  admission : Request
  digest : String
  coverage : Coverage
  deriving DecidableEq, Repr

structure PageCursor where
  binding : PageBinding
  offset : Nat
  deriving DecidableEq, Repr

/-- The attempt identity is persisted by the client before transport. A retry
of that attempt retains its identity; an explicit new attempt is distinct.
Native request keys domain-separate the complete demand's canonical encoding;
collision resistance and physical-reference decoding remain native premises. -/
structure PageDemand where
  admission : Request
  attempt : String
  cursor : Option PageCursor
  deriving DecidableEq, Repr

def PageDemand.wellFormed (demand : PageDemand) : Bool :=
  [demand.attempt, demand.admission.key, demand.admission.peer,
   demand.admission.requester, demand.admission.node, demand.admission.session].all
    (fun value => !value.isEmpty) &&
  match demand.cursor with
  | none => true
  | some cursor => cursor.binding.admission == demand.admission &&
      !cursor.binding.digest.isEmpty && cursor.offset != 0 && cursor.offset % pageLimit == 0 &&
      cursor.offset < cursor.binding.coverage.entries

structure WirePage where
  binding : PageBinding
  offset : Nat
  entries : List Entry
  next : Option PageCursor
  deriving DecidableEq, Repr

/-- Rendering follows source selection. Native hashing uses the full logical
snapshot's canonical preimage; collision resistance is an external crypto
premise, not a claim that every String uniquely encodes an arbitrary snapshot. -/
def Page.toWire (value : Page) (digest : String) : WirePage :=
  let binding := PageBinding.mk value.snapshot.admission digest value.snapshot.coverage
  ⟨binding, value.offset, value.entries, value.next.map fun cursor => ⟨binding, cursor.offset⟩⟩

inductive ReceiptPurpose where
  | transcriptV1
  | fileInventoryV2
  deriving DecidableEq, Repr

/-- `signatureVerified` is the existing NodeIdentity verifier's observation
over the exact versioned purpose, nonce, binding and full page payload. It is
not a field accepted from the wire or persisted without re-verification. -/
structure PageEnvelope where
  purpose : ReceiptPurpose
  requestKey : String
  signer : String
  signatureVerified : Bool
  page : WirePage
  deriving DecidableEq, Repr

def Entry.key (value : Entry) : DocId × String × String :=
  ⟨value.projection, value.change.root, value.change.path⟩

def Entry.orderCoherent (left right : Entry) : Bool :=
  if left.scope.node != right.scope.node || left.change.root != right.change.root ||
      left.change.path != right.change.path then true
  else match left.order, right.order with
    | .unresolved, .unresolved => true
    | .ordered li lc, .ordered ri rc => lc == rc && (left.tool == right.tool || li != ri)
    | _, _ => false

def coherentEntries (entries : List Entry) : Bool :=
  (entries.map Entry.key).Nodup &&
    entries.Pairwise (fun left right => left.tool ≤ right.tool) &&
    entries.all (fun entry => validScope entry.scope &&
      !entry.change.root.isEmpty && !entry.change.path.isEmpty && entry.order.wellFormed &&
      entry.requestCoverage.all (fun coverage => coverage.entries > 0)) &&
    entries.all (fun left => entries.all fun right =>
      left.orderCoherent right &&
      (left.request != right.request || left.scope != right.scope ||
        left.requestCoverage == right.requestCoverage) &&
      (left.tool != right.tool || (left.request == right.request && left.scope == right.scope &&
        left.projection == right.projection && left.observation == right.observation)) &&
      (left.projection != right.projection || (left.tool == right.tool &&
        left.request == right.request && left.scope == right.scope &&
        left.observation == right.observation)) &&
      (left.observation != right.observation || left.projection == right.projection))

def WirePage.wellFormed (value : WirePage) : Bool :=
  let following := value.offset + value.entries.length
  !value.binding.digest.isEmpty && value.entries.length ≤ pageLimit &&
    value.offset ≤ value.binding.coverage.entries &&
    value.entries.length == min pageLimit (value.binding.coverage.entries - value.offset) &&
    value.next == (if following < value.binding.coverage.entries then
      some ⟨value.binding, following⟩ else none) &&
    value.entries.all (fun entry => entry.order.within value.binding.coverage.entries &&
      entry.requestCoverage.all (fun coverage =>
        coverage.entries ≤ value.binding.coverage.entries &&
        coverage.pending ≤ value.binding.coverage.pending &&
        coverage.unknown ≤ value.binding.coverage.unknown)) &&
    coherentEntries value.entries

/-- Correlation precedes the existing signed receiver checks. The native
nonce comparison binds the complete demand's domain-separated key; it is not
an untrusted boolean field received from the peer. -/
def PageDemand.matchesPage (demand : PageDemand) (value : WirePage) (nonceMatches : Bool) : Bool :=
  demand.wellFormed && nonceMatches && value.binding.admission == demand.admission &&
    match demand.cursor with
    | none => value.offset == 0
    | some cursor => value.binding == cursor.binding && value.offset == cursor.offset

/-- The selected page is the source's immutable, signature-verified preparation.
Its exact demand, digest and entry index bind the image request to one physical
root and historical snapshot. A metadata receipt alone grants no image read. -/
structure ImageDemand where
  image : FileImages.Demand
  metadata : PageDemand
  digest : String
  entryIndex : Nat
  deriving DecidableEq, Repr

/-- Native ACP observations are obtained as the authenticated reader, in the
same source transaction as the canonical root and selected physical tool.
Authored requester fields and a readable public session cannot supply these
protected projection/image observations. These facts are not caller claims. -/
structure SelectedImageRead where
  scope : CanonicalOutput.Hydration.AuthorizationScope
  projection : DocId
  observation : DocId
  projectionVisible : Bool
  imageVisible : Bool
  projectionOwner : String
  imageOwner : String
  deriving DecidableEq, Repr

def selectImage (demand : ImageDemand) (catalog : Catalog) (page : PageEnvelope)
    (nonceMatches : Bool) (pageOwner : String) (read : SelectedImageRead)
    (tools : List FileImages.Tool) (images : List FileImages.Image) : FileImages.Selection :=
  if !FileImages.wellFormed demand.image || !decide (admits catalog demand.image.admission) ||
      demand.image.admission.authorizationScope != demand.metadata.admission.authorizationScope ||
      !page.signatureVerified || page.requestKey.isEmpty || page.purpose != .fileInventoryV2 ||
      page.signer != demand.image.admission.node || pageOwner != page.signer ||
      !demand.metadata.matchesPage page.page nonceMatches || !page.page.wellFormed ||
      demand.digest.isEmpty || demand.digest != page.page.binding.digest then .rejected
  else match page.page.entries[demand.entryIndex]? with
  | none => .rejected
  | some entry =>
      if read.scope != demand.image.admission.authorizationScope ||
          read.projection != entry.projection || read.observation != entry.observation ||
          !read.projectionVisible || !read.imageVisible ||
          read.projectionOwner != entry.scope.node || read.imageOwner != entry.scope.node ||
          entry.scope.node != demand.image.admission.node ||
          demand.image.toolDocId != toString entry.tool ||
          demand.image.requestDocId != toString entry.request then .rejected
      else match tools.filter (fun tool => tool.docId == demand.image.toolDocId),
          images.filter (fun image => image.toolDocId == demand.image.toolDocId) with
      | [tool], [image] =>
          if tool.accepted && tool.terminal && tool.scope == entry.scope &&
              tool.requestDocId == demand.image.requestDocId && image.scope == entry.scope &&
              image.docId == toString entry.observation &&
              image.toolDocId == tool.docId && image.requestDocId == tool.requestDocId &&
              image.contents.all (fun content => !content.docId.isEmpty && content.toolDocId == tool.docId && content.scope == entry.scope) then
            .selected image.docId
          else .rejected
      | _, _ => .rejected

theorem metadata_cannot_replace_image_permission (demand : ImageDemand) (catalog : Catalog)
    (page : PageEnvelope) (nonceMatches : Bool) (pageOwner : String) (read : SelectedImageRead)
    (tools : List FileImages.Tool) (images : List FileImages.Image)
    (denied : read.imageVisible = false) :
    selectImage demand catalog page nonceMatches pageOwner read tools images = .rejected := by
  simp only [selectImage]
  split
  · rfl
  · split
    · rfl
    · simp [denied]

inductive RefusalReason where
  | rejected | stale | unavailable
  deriving DecidableEq, Repr

def Error.refusal : Error → RefusalReason
  | .rejected => .rejected
  | .unknown => .unavailable

theorem invalid_source_is_rejected : Error.rejected.refusal = .rejected := rfl

theorem unavailable_source_is_not_empty : Error.unknown.refusal = .unavailable := rfl

end SessionHydration.FileInventory
