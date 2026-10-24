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
The native capture decoder owns path, hash, permission and byte validation.
Physical IDs are the existing model's abstract names for successfully decoded
native documents, never a way to accept empty or fabricated native `_docID`s.
The current owner comes from the transaction-bound ACP observation, not the
stored metadata JSON or its authored node/signer columns. -/
structure Projection where
  id : DocId
  observation : DocId
  tool : DocId
  request : DocId
  scope : FileImages.Scope
  /-- DefraDB's current authenticated document owner, not an authored JSON claim. -/
  owner : String
  changes : List Change
  deriving DecidableEq, Repr

def Observation.metadata (observation : Observation) (projectionId : DocId) : Projection :=
  ⟨projectionId, observation.id, observation.tool, observation.request,
    observation.scope, observation.scope.node, observation.changes.map CapturedChange.metadata⟩

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
      actual.accepted && actual.id == pre.callId && actual.request == pre.requestId &&
      actual.state == pre.state && observation.scope == actual.scope &&
      observation.tool == post.callId && observation.request == post.requestId &&
      !observation.changes.isEmpty then
    some (observation.metadata projectionId)
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

private def delivers (tool : Tool) (message : MessageEnvelope) : Bool :=
  message.header.request == some tool.request && message.header.role == .user &&
    message.header.publication == .toolDelivery tool.id &&
    (match message.blocks with
      | [.toolResult id _ _ _] => id == tool.id
      | _ => false)

private def uniquePosition (input : ClosureInput) (messages : List MessageEnvelope)
    (predicate : MessageEnvelope → Bool) : Option Nat :=
  match messages.filter (fun message =>
    (origin input (input.messages.length + 1) message).any predicate) with
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
  let admitted ← uniquePosition input selected (calls tool)
  let delivered ← uniquePosition input selected (delivers tool)
  if admitted < delivered then some ⟨tool.id, admitted, delivered⟩ else none

end Order

inductive ReadBoundary where
  | atomicExhaustive
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
  deriving DecidableEq

inductive Error where
  | rejected
  | unknown
  deriving DecidableEq, Repr

def validScope (scope : FileImages.Scope) : Bool :=
  [scope.requester, scope.node, scope.session].all (fun value => !value.isEmpty)

def Projection.matches (projection : Projection) (tool : Tool) : Bool :=
  projection.tool == tool.id && projection.request == tool.request &&
    projection.scope == tool.scope && projection.owner == tool.scope.node &&
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
private def assembleSnapshot (source : Source) (admission : Request) : Except Error Snapshot := do
  if source.boundary != .atomicExhaustive then .error .unknown else pure ()
  let selected ← selectedManifest source.catalog admission
  let toolIds := (selected.filter (fun key => key.collection == .agentToolCall)).map (·.id)
  let tools ← toolIds.mapM (toolFact source)
  let observed := tools.filterMap fun fact => match fact.capture with
    | .observed projection => some projection
    | _ => none
  if !(observed.map (·.id)).Nodup || !(observed.map (·.observation)).Nodup then
    .error .rejected
  else
    let input ← match closureInputFor source.catalog.closureInputs admission with
      | none => .error .unknown
      | some input => .ok input
    let initial : Snapshot := ⟨admission, selected, tools, []⟩
    .ok { initial with ordering := tools.map fun fact => Order.window initial input fact.tool }

def snapshot (source : Source) (admission : Request) : Except Error Snapshot := do
  if !decide (admits source.catalog admission) then .error .rejected else pure ()
  assembleSnapshot source admission

/-- DefraDB authenticates the HTTP actor. Its native node DID owns the NAC
policy and always admits that DID; this is not an enrollment or an authored
owner claim. The signer and requested source must be that same native owner.
Canonical closure selection and native document ACP still govern the read. -/
def operatorAdmitted (owner actor signer : String) (admission : Request) : Bool :=
  !owner.isEmpty && actor == owner && signer == owner && admission.node == owner

def operatorSnapshot (source : Source) (owner actor signer : String) (admission : Request) :
    Except Error Snapshot := do
  if !operatorAdmitted owner actor signer admission then .error .rejected else pure ()
  let sessions := source.catalog.sessions.filter (fun session =>
    session.node == admission.node && session.session == admission.session)
  if sessions.card != 1 then .error .unknown
  else if !decide (∀ session ∈ sessions, session.requester = admission.requester) then .error .unknown
  else if !decide (ownedSession admission ∈ sessions) then .error .rejected
  else assembleSnapshot source admission

theorem anonymous_operator_is_denied (owner signer : String) (admission : Request) :
    operatorAdmitted owner "" signer admission = false := by
  by_cases h : owner = ""
  · subst owner; rfl
  · simp [operatorAdmitted, Ne.symm h]

theorem denied_operator_cannot_select (source : Source)
    (owner actor signer : String) (admission : Request)
    (denied : operatorAdmitted owner actor signer admission = false) :
    operatorSnapshot source owner actor signer admission = .error .rejected := by
  simp [operatorSnapshot, denied, Bind.bind, Except.bind]

structure Entry where
  tool : DocId
  request : DocId
  scope : FileImages.Scope
  projection : DocId
  observation : DocId
  change : Change
  order : Order.Position := .unresolved
  deriving DecidableEq, Repr

def Snapshot.rawEntries (value : Snapshot) : List Entry :=
  value.tools.flatMap fun fact => match fact.capture with
    | .observed projection => projection.changes.map fun change =>
        ⟨fact.tool.id, fact.tool.request, fact.tool.scope,
          projection.id, projection.observation, change, .unresolved⟩
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

structure Coverage where
  entries : Nat
  pending : Nat
  unknown : Nat
  deriving DecidableEq, Repr

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

def sameDemand (left right : PageDemand) : Bool := left == right

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
  | fileInventoryV3
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

structure ReceivedPage where
  requestKey : String
  envelope : PageEnvelope
  deriving DecidableEq, Repr

structure Prefix where
  binding : PageBinding
  entries : List Entry
  complete : Bool
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
      !entry.change.root.isEmpty && !entry.change.path.isEmpty && entry.order.wellFormed) &&
    entries.all (fun left => entries.all fun right =>
      left.orderCoherent right &&
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
    value.entries.all (fun entry => entry.order.within value.binding.coverage.entries) &&
    coherentEntries value.entries

/-- Correlation precedes the existing signed receiver checks. The native
nonce comparison binds the complete demand's domain-separated key; it is not
an untrusted boolean field received from the peer. -/
def PageDemand.matchesPage (demand : PageDemand) (value : WirePage) (nonceMatches : Bool) : Bool :=
  demand.wellFormed && nonceMatches && value.binding.admission == demand.admission &&
    match demand.cursor with
    | none => value.offset == 0
    | some cursor => value.binding == cursor.binding && value.offset == cursor.offset

/-- The existing admission and enrollment owners supply the two current
observations. Issuance withholds the signed value if authorization changes
while native signing is in flight; this adds no permission or image grant. -/
def mayIssue (value : WirePage) (nonce signer : String) (before after : Bool) : Bool :=
  value.wellFormed && !nonce.isEmpty && signer == value.binding.admission.node && before && after

theorem revoked_after_signing_cannot_issue (value : WirePage) (nonce signer : String) (before : Bool) :
    mayIssue value nonce signer before false = false := by simp [mayIssue]

theorem revoked_before_signing_cannot_issue (value : WirePage) (nonce signer : String) (after : Bool) :
    mayIssue value nonce signer false after = false := by simp [mayIssue]

inductive RefusalReason where
  | rejected | stale | unavailable
  deriving DecidableEq, Repr

def Error.refusal : Error → RefusalReason
  | .rejected => .rejected
  | .unknown => .unavailable

theorem invalid_source_is_rejected : Error.rejected.refusal = .rejected := rfl

theorem unavailable_source_is_not_empty : Error.unknown.refusal = .unavailable := rfl

/-- A refusal carries only the client's exact demand and a fixed reason. It
contains no selected membership, metadata or image grant. Native verification
binds its disjoint versioned purpose and full demand, including the cursor. -/
def acceptsRefusal (expected actual : PageDemand) (version nonceMatches : Bool)
    (signer : String) (signatureVerified : Bool) : Bool :=
  expected.wellFormed && expected == actual && version && nonceMatches &&
    signer == expected.admission.node && signatureVerified

inductive Reply where
  | page (value : WirePage)
  | refused (reason : RefusalReason)
  deriving DecidableEq, Repr

/-- Prior replies have been decoded, correlated and signature-verified for the
exact demand, and their native document owner is the source. The local journal
is not a transfer grant; delivery must still check current authorization. -/
def recordReply (prior : Option Reply) (candidate : Reply)
    (write : TerminalWriteResult) : Option Reply :=
  match prior with
  | some outcome => some outcome
  | none => if write == .committed then some candidate else none

theorem recorded_reply_is_immutable (prior candidate : Reply) (write : TerminalWriteResult) :
    recordReply (some prior) candidate write = some prior := rfl

theorem failed_reply_is_not_retained (candidate : Reply) :
    recordReply none candidate .failed = none := rfl

theorem stale_reply_is_not_upgraded (candidate : Reply) :
    recordReply (some (.refused .stale)) candidate .committed = some (.refused .stale) := rfl

/-- Preparation is an immutable source-local choice for one exact demand.
The native owner verifies its signature, correlation and current document
owner before observing a prior choice. A retry does not reselect from a newer
snapshot, and preparation does not establish delivery or terminal settlement. -/
def preparePage (prior : Option WirePage) (candidate : WirePage)
    (write : TerminalWriteResult) : Option WirePage :=
  (recordReply (prior.map Reply.page) (.page candidate) write).bind fun reply =>
    match reply with
    | .page value => some value
    | .refused _ => none

theorem prepared_page_is_immutable (prior candidate : WirePage) (write : TerminalWriteResult) :
    preparePage (some prior) candidate write = some prior := rfl

theorem preparation_rollback_retains_nothing (candidate : WirePage) :
    preparePage none candidate .failed = none := rfl

/-- A page is prepared locally before explicit delivery. Only a confirmed
transfer with both current authority observations may settle a page; a lost
acknowledgment keeps the attempt pending. Refusals carry no file metadata. -/
def completeReply (candidate : Reply) (before after : Bool) (delivery : DeliveryResult)
    (write : TerminalWriteResult) : Option Reply :=
  if write != .committed then none
  else match candidate with
  | .refused reason => some (.refused reason)
  | .page value =>
      if !before || !after then some (.refused .rejected)
      else if delivery == .indeterminate then none else some (.page value)

theorem unacknowledged_page_stays_pending (value : WirePage) :
    completeReply (.page value) true true .indeterminate .committed = none := rfl

theorem revoked_page_cannot_settle_as_page (value : WirePage) (before : Bool)
    (delivery : DeliveryResult) :
    completeReply (.page value) before false delivery .committed = some (.refused .rejected) := by
  cases before <;> rfl

/-- The pointer has a separate signed purpose and contains only a demand and
one physical page document ID. A successful native verification authorizes a
local replica read of that exact metadata document, never any image document. -/
def acceptsPageGrant (expected actual : PageDemand) (version : Bool)
    (document signer : String) (signatureVerified : Bool) : Bool :=
  expected.wellFormed && expected == actual && version && !document.isEmpty &&
    signer == expected.admission.node && signatureVerified

/-- All authorization remains with the existing source/identity owners. This
receiver checks their signed metadata representation, not another access
policy. Fork entries retain the attested actual scope, including origins. -/
def advance (admission : Request) (previous : Option Prefix) (received : ReceivedPage) : Option Prefix :=
  let envelope := received.envelope
  let value := envelope.page
  if !envelope.signatureVerified || envelope.purpose != .fileInventoryV3 ||
      envelope.signer != admission.node || received.requestKey.isEmpty ||
      envelope.requestKey != received.requestKey || value.binding.admission != admission ||
      !value.wellFormed then none
  else match previous with
  | none =>
      if value.offset != 0 then none
      else some ⟨value.binding, value.entries,
        value.entries.length == value.binding.coverage.entries⟩
  | some prior =>
      if prior.complete || prior.binding != value.binding ||
          value.offset != prior.entries.length ||
          !coherentEntries (prior.entries ++ value.entries) then none
      else some ⟨value.binding, prior.entries ++ value.entries,
        prior.entries.length + value.entries.length == value.binding.coverage.entries⟩

def validatePrefix (admission : Request) :
    Option Prefix → List ReceivedPage → Option Prefix
  | prior, [] => prior
  | prior, head :: tail => do
      let next ← advance admission prior head
      validatePrefix admission (some next) tail

/-- Stored page bytes are re-verified by the native decoder before entering
this owner. There is no full snapshot or unsigned retained entry cache here. -/
structure Receiver where
  admission : Request
  pages : List ReceivedPage
  deriving DecidableEq, Repr

def Receiver.validated (state : Receiver) : Option Prefix :=
  if !(state.pages.map (·.requestKey)).Nodup then none
  else validatePrefix state.admission none state.pages

def receive (state : Receiver) (requestKey : String) (envelope : PageEnvelope) : Option Receiver :=
  let next := Receiver.mk state.admission (state.pages ++ [⟨requestKey, envelope⟩])
  if next.validated.isSome then some next else none

def Receiver.complete (state : Receiver) : Bool := state.validated.any (·.complete)

def knownInventory (state : Receiver) : Bool :=
  state.validated.any fun prior => prior.complete &&
    prior.binding.coverage.pending == 0 && prior.binding.coverage.unknown == 0

theorem oversized_page_cannot_advance (admission : Request) (prior : Option Prefix)
    (received : ReceivedPage) (h : pageLimit < received.envelope.page.entries.length) :
    advance admission prior received = none := by
  simp [advance, WirePage.wellFormed, Nat.not_le_of_lt h]

theorem malformed_page_cannot_advance (admission : Request) (prior : Option Prefix)
    (received : ReceivedPage) (h : received.envelope.page.wellFormed = false) :
    advance admission prior received = none := by simp [advance, h]

theorem unverified_page_cannot_advance (admission : Request) (prior : Option Prefix)
    (received : ReceivedPage) (h : received.envelope.signatureVerified = false) :
    advance admission prior received = none := by simp [advance, h]

theorem contradictory_binding_cannot_advance (admission : Request) (prior : Prefix)
    (received : ReceivedPage) (h : prior.binding ≠ received.envelope.page.binding) :
    advance admission (some prior) received = none := by simp [advance, h]

theorem empty_history_is_not_complete (admission : Request) :
    (Receiver.mk admission []).complete = false := by
  simp [Receiver.complete, Receiver.validated, validatePrefix]

theorem completed_advance_has_exact_count (admission : Request) (previous : Option Prefix)
    (received : ReceivedPage) (after : Prefix)
    (h : advance admission previous received = some after) (hc : after.complete = true) :
    after.entries.length = after.binding.coverage.entries := by
  cases previous with
  | none =>
    simp only [advance] at h
    split at h
    · contradiction
    · split at h
      · contradiction
      · injection h with heq
        subst after
        simpa only [beq_iff_eq] using hc
  | some prior =>
    simp only [advance] at h
    split at h
    · contradiction
    · split at h
      · contradiction
      · injection h with heq
        subst after
        simpa only [List.length_append, beq_iff_eq] using hc

/-- A client retains signed pages for one durable attempt. The native decoder
rechecks the complete demand and signature of every stored page or refusal.
Neither the prior completed view nor a partially received view grants images. -/
structure ClientAttempt where
  id : String
  receiver : Receiver
  refusal : Option RefusalReason := none
  deriving DecidableEq, Repr

def ClientAttempt.pending (state : ClientAttempt) : Option PageDemand :=
  if state.refusal.isSome || state.receiver.complete then none
  else some ⟨state.receiver.admission, state.id,
    state.receiver.pages.getLast?.bind (·.envelope.page.next)⟩

structure ClientProgress where
  active : ClientAttempt
  previous : Option ClientAttempt := none
  retired : List String := []
  deriving DecidableEq, Repr

/-- A retained scope selects previously verified local observations only. It
does not authorize fetching metadata or images. Multiple physical sessions or
requesters for one logical scope remain ambiguous rather than choosing a latest
row. Native index columns must agree with the protected progress admission. -/
def retainedScopeMatches (peer node session : String) (admission : Request) : Bool :=
  admission.peer == peer && admission.node == node && admission.session == session

def selectRetainedScope (peer node session : String) (rows : List Request) :
    Except String (Option Request) :=
  if peer.isEmpty || node.isEmpty || session.isEmpty then .error "invalid"
  else match rows.filter (retainedScopeMatches peer node session) with
    | [] => .ok none
    | [admission] => .ok (some admission)
    | _ => .error "ambiguous"

theorem duplicate_retained_scope_is_ambiguous (peer node session : String)
    (admission : Request) (valid : (peer.isEmpty || node.isEmpty || session.isEmpty) = false)
    (selected : retainedScopeMatches peer node session admission = true) :
    selectRetainedScope peer node session [admission, admission] = .error "ambiguous" := by
  simp [selectRetainedScope, valid, selected]

def beginClient (admission : Request) (attempt : String) : ClientProgress :=
  ⟨⟨attempt, ⟨admission, []⟩, none⟩, none, []⟩

/-- An ordinary read resumes the persisted attempt. The candidate identity is
used only when the local transaction observes no prior progress. -/
def resumeClient (prior : Option ClientProgress) (admission : Request)
    (candidate : String) : ClientProgress :=
  prior.getD (beginClient admission candidate)

theorem resume_keeps_active_attempt (prior : ClientProgress) (admission : Request)
    (candidate : String) : resumeClient (some prior) admission candidate = prior := rfl

/-- Repeating the same explicit attempt is a retry. Starting a distinct one
retains the last complete signed view while new enumeration remains pending. -/
def restartClient (state : ClientProgress) (attempt : String) : Option ClientProgress :=
  if attempt.isEmpty then none
  else if state.active.id == attempt then some state
  else if state.retired.contains attempt then none
  else some ⟨⟨attempt, ⟨state.active.receiver.admission, []⟩, none⟩,
    (if state.active.receiver.complete then some state.active else state.previous),
    state.retired ++ [state.active.id]⟩

def clientPage (state : ClientProgress) (attempt requestKey : String)
    (envelope : PageEnvelope) (nonceMatches : Bool) : Option ClientProgress := do
  if !nonceMatches || state.active.id != attempt then none else pure ()
  let received := ReceivedPage.mk requestKey envelope
  if state.active.receiver.pages.contains received then some state else do
    let pending ← state.active.pending
    if !pending.matchesPage envelope.page nonceMatches then none else pure ()
    let receiver ← receive state.active.receiver requestKey envelope
    some {state with active := {state.active with receiver := receiver}}

/-- Native refusal verification supplies exact-demand correlation before this
transition. A verified terminal refusal never extends or completes a prefix. -/
def clientRefusal (state : ClientProgress) (attempt : String)
    (reason : RefusalReason) (verified : Bool) : Option ClientProgress :=
  if !verified || state.active.id != attempt then none
  else if state.active.refusal == some reason then some state
  else if state.active.pending.isNone then none
  else some {state with active := {state.active with refusal := some reason}}

def commitClient (prior candidate : ClientProgress) (write : TerminalWriteResult) : ClientProgress :=
  if write == .committed then candidate else prior

theorem client_rollback_preserves_attempt (prior candidate : ClientProgress) :
    commitClient prior candidate .failed = prior := rfl

theorem restarting_same_client_attempt_is_retry (state : ClientProgress)
    (h : state.active.id.isEmpty = false) :
    restartClient state state.active.id = some state := by
  simp [restartClient, h]

theorem retired_client_attempt_cannot_restart (state : ClientProgress) (attempt : String)
    (h : state.active.id ≠ attempt) (retired : state.retired.contains attempt = true) :
    restartClient state attempt = none := by
  have mem : attempt ∈ state.retired := by simpa using retired
  simp [restartClient, h, mem]

theorem wrong_nonce_cannot_replay_client_page (state : ClientProgress)
    (attempt key : String) (envelope : PageEnvelope) :
    clientPage state attempt key envelope false = none := by
  simp [clientPage]

theorem refused_client_has_no_next_page (state : ClientAttempt)
    (h : state.refusal.isSome = true) : state.pending = none := by
  simp [ClientAttempt.pending, h]

end SessionHydration.FileInventory
