import Proofs.Conformance.TaskLineage
import Proofs.Conformance.QuestionReply
import Proofs.Conformance.RuntimeInput
import Proofs.Clients.ClientShell.Timeline
import Proofs.Session.SessionQueue.Archive
import Proofs.Conformance.ContractCases.SessionDocuments
import Proofs.Conformance.ContractCases.Types
import Lean
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.Contracts
open Lean Conformance.SessionDocuments

inductive PluginJobOpeningAttempt where
  | valid
  | wrongPin
  | failedTransaction
  deriving ToJson

/-- What each staged opening attempt presents: a spawn key pinned to another
job root, or a transaction that aborts after staging its writes. -/
def PluginJobOpeningAttempt.opening : PluginJobOpeningAttempt → AgentSession.PluginJobOpening
  | .valid => ⟨true, true, true, true, true⟩
  | .wrongPin => ⟨true, true, false, true, true⟩
  | .failedTransaction => ⟨true, true, true, true, false⟩

structure PluginJobOpeningInput where
  name : PluginJobOpeningAttempt
  deriving ToJson

structure PluginJobOpeningOutput where
  commits : Bool
  deriving ToJson

@[conformance_family]
def pluginJobOpening : Conformance.Family PluginJobOpeningInput PluginJobOpeningOutput where
  inputs := [.valid, .wrongPin, .failedTransaction].map PluginJobOpeningInput.mk
  model := fun i => ⟨AgentSession.pluginJobOpeningCommits i.name.opening⟩

def scopeJson (s : AgentSession.Scope) : Json := Json.mkObj
  [("node", toJson s.node), ("session", toJson s.session), ("requester", toJson s.requester)]
def requestJson (r : AgentSession.RequestFact) : Json := Json.mkObj
  [("purpose", toJson r.purpose.toWire), ("scope", scopeJson r.scope), ("agent", toJson r.agent),
   ("created_at", toJson r.createdAt), ("doc_id", toJson r.observed.docId),
   ("request_id", toJson r.observed.requestId), ("state", toJson r.observed.state.toDefraDB),
   ("terminalized_at", toJson r.terminalizedAt)]
private def provenanceJson (p : AgentSession.Provenance) : Json := Json.mkObj
  [("task_id", toJson p.task),
   ("parent_request_doc_id", toJson p.parentRequestDoc),
   ("started_by", p.startedBy.map scopeJson |>.getD Json.null),
   ("opening_request_doc_id", toJson p.openingRequestDoc),
   ("fork", p.fork.map (fun f => Json.mkObj
     [("source_session_id", toJson f.sourceSession), ("at_user_turn", toJson f.atUserTurn)])
     |>.getD Json.null)]
def documentJson (d : AgentSession.Document) : Json := Json.mkObj
  [("scope", scopeJson d.scope), ("agent", toJson d.agent),
   ("created_at", toJson d.createdAt), ("closed_at", toJson d.closedAt),
   ("stopped_at", toJson d.stoppedAt), ("archived_at", toJson d.archivedAt), ("archive_revision", toJson d.archiveRevision),
   ("provenance", d.provenance.map provenanceJson |>.getD Json.null),
   ("tags", toJson d.tags), ("title", d.title.map (fun t => Json.mkObj
      [("text", toJson t.text), ("source", toJson t.source.toWireName)]) |>.getD Json.null),
   ("observation", d.observation.map (fun o => Json.mkObj
      [("activity", toJson o.activity), ("preview", toJson o.preview),
       ("latest", o.latest.map (fun r => Json.mkObj
         [("doc_id", toJson r.docId), ("request_id", toJson r.requestId),
          ("state", toJson r.state.toDefraDB)]) |>.getD Json.null)]) |>.getD Json.null)]

private def retryStateJson (s : SessionState) : Json := Json.mkObj
  [("session_id", toJson s.sessionId), ("agent", toJson s.agentDid),
   ("latest", toJson s.latest), ("requests", toJson ([1, 3].filter
     (fun id => decide (id ∈ s.requestIds)) |>.map (fun id => Json.mkObj
       [("request_id", toJson id), ("state", toJson (s.ctx id).state.toDefraDB),
        ("origin", toJson (s.ctx id).origin.toDefraDB),
        ("admission", toJson (Conformance.ContractCases.admissionName (s.ctx id).admission)),
        ("deadline", toJson (s.ctx id).deadline), ("current_time", toJson (s.ctx id).currentTime),
        ("retry_count", toJson (s.ctx id).retryCount), ("max_retries", toJson (s.ctx id).maxRetries)])))]
private def retryJson (name : String) (rows : List AgentSession.RequestFact) (parentDoc : Nat) : Json :=
  Json.mkObj [("name", toJson name), ("session", documentJson indexed),
    ("before", retryStateJson retryState), ("rows", toJson (rows.map requestJson)),
    ("parent_doc", toJson parentDoc), ("failed_id", toJson (1 : Nat)), ("new_id", toJson (3 : Nat)),
    ("after", (SessionState.retryFromRows? retryState indexed rows parentDoc 1 3).map
      retryStateJson |>.getD Json.null)]
private def roleName : CanonicalOutput.MessageRole → String
  | .system => "system"
  | .user => "user"
  | .assistant => "assistant"
private def editBlockName {α : Type} : CanonicalOutput.MessageBlock α → String
  | .text _ => "text"
  | .reasoning _ _ => "reasoning"
  | .toolCall _ _ _ _ _ _ _ => "toolCall"
  | .toolResult _ _ _ _ => "toolResult"
  | .media _ => "media"

structure TitleWire where
  text : String
  source : String
  deriving ToJson

structure ForkWire where
  sourceSessionId : Nat
  atUserTurn : Nat
  deriving ToJson

structure ProvenanceWire where
  taskId : Option Nat
  parentRequestDocId : Option Nat
  startedBy : Option AgentSession.Scope
  openingRequestDocId : Option Nat
  fork : Option ForkWire
  deriving ToJson

structure LatestWire where
  docId : Nat
  requestId : Nat
  state : RequestState
  deriving ToJson

structure ObservationWire where
  activity : Nat
  preview : Option String
  latest : Option LatestWire
  deriving ToJson

/-- A session document as the wire carries it: identifiers and times are
the model's numbers, a title source and request state their wire names. -/
structure DocumentWire where
  scope : AgentSession.Scope
  agent : Nat
  createdAt : Nat
  closedAt : Option Nat
  stoppedAt : Option Nat
  archivedAt : Option Nat
  archiveRevision : Nat
  provenance : Option ProvenanceWire
  tags : List String
  title : Option TitleWire
  observation : Option ObservationWire
  deriving ToJson

def DocumentWire.ofModel (d : AgentSession.Document) : DocumentWire :=
  { scope := d.scope, agent := d.agent, createdAt := d.createdAt, closedAt := d.closedAt
    stoppedAt := d.stoppedAt, archivedAt := d.archivedAt, archiveRevision := d.archiveRevision
    provenance := d.provenance.map fun p =>
      { taskId := p.task, parentRequestDocId := p.parentRequestDoc, startedBy := p.startedBy
        openingRequestDocId := p.openingRequestDoc
        fork := p.fork.map fun f => { sourceSessionId := f.sourceSession, atUserTurn := f.atUserTurn } }
    tags := d.tags
    title := d.title.map fun t => { text := t.text, source := t.source.toWireName }
    observation := d.observation.map fun o =>
      { activity := o.activity, preview := o.preview
        latest := o.latest.map fun r => { docId := r.docId, requestId := r.requestId, state := r.state } } }

def titleSourceOfWire : String → AgentSession.TitleSource
  | "placeholder" => .placeholder
  | "generated" => .generated
  | "task" => .task
  | _ => .user

def DocumentWire.toModel (w : DocumentWire) : AgentSession.Document :=
  { scope := w.scope, agent := w.agent, createdAt := w.createdAt, closedAt := w.closedAt
    stoppedAt := w.stoppedAt, archivedAt := w.archivedAt, archiveRevision := w.archiveRevision
    provenance := w.provenance.map fun p =>
      { task := p.taskId, parentRequestDoc := p.parentRequestDocId, startedBy := p.startedBy
        openingRequestDoc := p.openingRequestDocId
        fork := p.fork.map fun f => ⟨f.sourceSessionId, f.atUserTurn⟩ }
    tags := w.tags
    title := w.title.map fun t => ⟨t.text, titleSourceOfWire t.source⟩
    observation := w.observation.map fun o =>
      { activity := o.activity, preview := o.preview
        latest := o.latest.map fun r => ⟨r.docId, r.requestId, r.state⟩ } }

private def archiveDecisionName : AgentSession.Archive.Decision → String
  | .denied => "denied"
  | .unchanged => "unchanged"
  | .conflict => "conflict"
  | .exhausted => "exhausted"
  | .applied => "applied"

def archiveLimit : Nat := 9223372036854775807

structure ArchiveInput where
  before : DocumentWire
  authorized : Bool
  expectedRevision : Nat
  archived : Bool
  now : Nat
  limit : Nat
  deriving ToJson

def archiveInputs : List ArchiveInput := do
  let (archivedAt, revision) ← ([(none, 0), (some 3, 1), (none, 2), (some 3, archiveLimit)] :
    List (Option Nat × Nat))
  let lifecycle ← [0, 1, 2]
  let authorized ← [false, true]
  let expected ← [0, 1, 2, archiveLimit]
  let archived ← [false, true]
  let before := { indexed with
    archivedAt, archiveRevision := revision
    closedAt := if lifecycle = 1 then some 5 else none
    stoppedAt := if lifecycle = 2 then some 6 else none
    observation := indexed.observation.map fun o => { o with
      latest := some { newerRequest.observed with
        state := if lifecycle = 0 then .processing else .failed } } }
  pure { before := DocumentWire.ofModel before, authorized, expectedRevision := expected, archived
         now := 7, limit := archiveLimit }

structure ArchiveOutput where
  decision : String
  after : DocumentWire
  activeVisible : Bool
  archivedVisible : Bool
  allVisible : Bool
  deriving ToJson

def archiveOutcome (i : ArchiveInput) : ArchiveOutput :=
  let (decision, after) := AgentSession.Archive.apply i.before.toModel i.authorized
    i.expectedRevision i.archived i.now i.limit
  { decision := archiveDecisionName decision, after := DocumentWire.ofModel after
    activeVisible := AgentSession.Archive.visible .active after
    archivedVisible := AgentSession.Archive.visible .archived after
    allVisible := AgentSession.Archive.visible .all after }

@[conformance_family]
def archiveTransition : Family ArchiveInput ArchiveOutput where
  inputs := archiveInputs.filter (·.authorized)
  model := archiveOutcome

example : archiveTransition.inputs.any fun i =>
    i.before.archiveRevision == 2 && i.expectedRevision == 0 && i.archived := by native_decide

/-- model-production-gap: archive authorization is DefraDB ACP on the
caller's transaction before the revision transition runs; no production
scenario replays these unauthorized archive writes. -/
@[conformance_family]
def archiveTransitionUnreached : Family ArchiveInput ArchiveOutput where
  inputs := archiveInputs.filter fun i => !i.authorized
  model := archiveOutcome

structure ArchiveStep where
  expectedRevision : Nat
  archived : Bool
  deriving ToJson

structure ArchiveTrace where
  steps : List ArchiveStep
  deriving ToJson

structure ArchiveReceipt where
  decision : String
  revision : Nat
  archived : Bool
  deriving ToJson

/-- Archive commands submitted in order to one runtime session by two
clients; each receipt is the outcome of one authorized archive transition. -/
@[conformance_family]
def archiveReceipts : Family ArchiveTrace (List ArchiveReceipt) where
  inputs := [{ steps := [⟨0, true⟩, ⟨0, false⟩, ⟨0, true⟩, ⟨1, false⟩, ⟨0, true⟩] }]
  model := fun trace =>
    (trace.steps.foldl (fun (state : AgentSession.Document × List ArchiveReceipt) step =>
      let (decision, after) := AgentSession.Archive.apply state.1 true step.expectedRevision
        step.archived 7 archiveLimit
      (after, state.2 ++ [{ decision := archiveDecisionName decision
                            revision := after.archiveRevision
                            archived := after.archivedAt.isSome }]))
      ({ indexed with archivedAt := none, archiveRevision := 0 }, [])).2

structure StarterIndex where
  node : Nat
  session : Nat
  deriving ToJson

structure StarterInput where
  name : String
  index : Option StarterIndex
  physical : Nat
  recorded : Option AgentSession.Scope
  parent : Option Nat
  signed : Bool
  deriving ToJson

@[conformance_family]
def starterVerification : Family StarterInput Bool where
  inputs :=
    let source : AgentSession.Scope := ⟨5, 6, some 7⟩
    [ ("valid", some (5, 6), 8, some source, some 11, true),
      ("physical", some (5, 6), 9, some source, some 11, true),
      ("index", some (5, 7), 8, some source, some 11, true),
      ("source", some (5, 7), 8, some ⟨5, 7, some 7⟩, some 11, true),
      ("parent", some (5, 6), 8, some source, some 12, true),
      ("signature", some (5, 6), 8, some source, some 11, false),
      ("starter_removed", some (5, 6), 8, none, some 11, true),
      ("starter_removed_parent_rewritten", some (5, 6), 8, none, some 12, true),
      ("unindexed", none, 8, some source, some 11, true) ].map
      fun (name, index, physical, recorded, parent, signed) =>
        { name, index := index.map fun (node, session) => { node, session }, physical, recorded
          parent, signed }
  model := fun i =>
    (AgentSession.observedStarter (i.index.map fun index => (index.node, index.session))
      (some starterReceipt) i.physical i.recorded i.parent i.signed).isSome

example : starterVerification.inputs.any fun i =>
    i.name == "signature" && !starterVerification.model i := by native_decide


def sessionDocumentReferenceEncodingJson : String :=
  (toJson "interned collection plus _docID, not raw _docID").compress

def sessionDocumentRetryCasesJson : String :=
  (toJson [
      retryJson "exact_parent" [old] 101,
      retryJson "newer_authoritative_row" [old, newerRequest] 101,
      retryJson "wrong_physical_parent" [old] 999,
      retryJson "existing_candidate_missing_from_auxiliary_projection" [old, olderExistingCandidate] 101]).compress

structure RequestWire where
  purpose : String
  scope : AgentSession.Scope
  agent : Nat
  createdAt : Nat
  docId : Nat
  requestId : Nat
  state : RequestState
  terminalizedAt : Option Nat
  deriving ToJson

def RequestWire.ofModel (r : AgentSession.RequestFact) : RequestWire :=
  { purpose := r.purpose.toWire, scope := r.scope, agent := r.agent, createdAt := r.createdAt
    docId := r.observed.docId, requestId := r.observed.requestId, state := r.observed.state
    terminalizedAt := r.terminalizedAt }

def RequestWire.toModel (w : RequestWire) : AgentSession.RequestFact :=
  { purpose := (RequestPurpose.fromWire? w.purpose).getD .normal, scope := w.scope, agent := w.agent
    createdAt := w.createdAt, observed := ⟨w.docId, w.requestId, w.state⟩
    terminalizedAt := w.terminalizedAt }

structure SelectionInput where
  name : String
  requests : List RequestWire
  node : Nat
  session : Nat
  requesterScoped : Bool
  requester : Option Nat
  deriving ToJson

structure SelectionOutput where
  selected : Option RequestWire
  deriving ToJson

def selectionInput (name : String) (rows : List AgentSession.RequestFact)
    (node session : Nat) (requester : Option (Option Nat)) : SelectionInput :=
  { name, requests := rows.map RequestWire.ofModel, node, session
    requesterScoped := requester.isSome, requester := requester.getD none }

def selectionInputs : List SelectionInput :=
  [ selectionInput "timestamp_tie_forward" [old, newerRequest] 1 10 none,
    selectionInput "timestamp_tie_reverse" [newerRequest, old] 1 10 none,
    selectionInput "newer_title_preserves_normal_head" [titleRequest, old] 1 10 none,
    selectionInput "title_only_has_no_public_head" [titleRequest] 1 10 none,
    selectionInput "absent_requester_exact" [old] 1 10 (some none),
    selectionInput "foreign_agent" [old] 9 10 none,
    selectionInput "foreign_session" [old] 1 9 none,
    selectionInput "same_second_claim_after_completion" [finishedPredecessor, sameSecondClaim] 1 10 none,
    selectionInput "same_second_claim_after_completion_reverse" [sameSecondClaim, finishedPredecessor] 1 10 none,
    selectionInput "same_second_later_terminalization" [finishedPredecessor, sameSecondCompletion] 1 10 none,
    selectionInput "same_second_later_terminalization_reverse" [sameSecondCompletion, finishedPredecessor] 1 10 none,
    selectionInput "same_second_live_outranks_later_refusal" [sameSecondClaim, sameSecondRefusal] 1 10 none,
    selectionInput "earlier_second_live_loses" [{ sameSecondClaim with createdAt := 1 }, finishedPredecessor] 1 10 none ]

def selectionOutcome (i : SelectionInput) : SelectionOutput :=
  { selected := (AgentSession.latest (i.requests.map RequestWire.toModel) i.node i.session
      (if i.requesterScoped then some i.requester else none)).map RequestWire.ofModel }

@[conformance_family]
def sessionSelection : Family SelectionInput SelectionOutput where
  inputs := selectionInputs
  model := selectionOutcome

/-- The desktop client store keeps request rows without their terminalization
time and selects within one node's session, so it replays the title-purpose
selections, whose rows differ only in purpose and creation time. -/
@[conformance_family]
def sessionSelectionClientStore : Family SelectionInput SelectionOutput where
  inputs := selectionInputs.filter fun i =>
    i.name ∈ ["newer_title_preserves_normal_head", "title_only_has_no_public_head"]
  model := selectionOutcome

structure EditMessage where
  key : String
  sequence : Nat
  role : String
  blocks : List String
  deriving ToJson

/-- The wire projection of a modeled edit-anchor case: each message's key,
sequence, role and block kinds. The model reads the case's own history. -/
structure EditAnchorInput where
  name : String
  key : String
  messages : List EditMessage
  deriving ToJson

def EditAnchorInput.ofCase (witness : EditAnchorCase) : EditAnchorInput :=
  { name := witness.name, key := witness.key
    messages := witness.source.messages.map fun message =>
      { key := message.key, sequence := message.sequence, role := roleName message.header.role
        blocks := message.blocks.map editBlockName } }

def EditAnchorInput.source? (i : EditAnchorInput) : Option SessionFork.History :=
  (editAnchorCases.find? (·.name == i.name)).map (·.source)

structure EditAnchorCut where
  cut : Option Nat
  deriving ToJson

@[conformance_family]
def editAnchorCut : Family EditAnchorInput EditAnchorCut where
  inputs := editAnchorCases.map EditAnchorInput.ofCase
  model := fun i => { cut := i.source?.bind fun source => SessionFork.editCut source i.key }

structure EditAnchorRetained where
  retainedCount : Option Nat
  deriving ToJson

def editAnchorRetained (i : EditAnchorInput) : EditAnchorRetained :=
  { retainedCount := i.source?.bind fun source =>
      (SessionFork.editCut source i.key).map fun cut =>
        (SessionFork.copyPrefix source child remap childKey cut).messages.length }

/-- The fork scenario publishes signed user-text turns only, so only a history
made of user messages can be built on a served node. -/
@[conformance_family]
def editAnchorFork : Family EditAnchorInput EditAnchorRetained where
  inputs := (editAnchorCases.filter fun c => c.name ∈ ["three_user_turns"]).map EditAnchorInput.ofCase
  model := editAnchorRetained

/-- model-production-gap: the served fork fixture publishes user-text turns
only; histories with assistant, tool-result or empty user messages, duplicate
keys or anchors that no message carries are not built by any production
scenario, so their retained prefix is not replayed. -/
@[conformance_family]
def editAnchorForkUnreached : Family EditAnchorInput EditAnchorRetained where
  inputs := (editAnchorCases.filter fun c => c.name ∉ ["three_user_turns"]).map EditAnchorInput.ofCase
  model := editAnchorRetained

deriving instance ToJson for SessionFork.EditForkBinding, SessionFork.EditForkDecision

structure EditForkInput where
  name : String
  requested : SessionFork.EditForkBinding
  retainedKind : String
  retained : Option SessionFork.EditForkBinding
  deriving ToJson

def EditForkInput.retainedFork (i : EditForkInput) : SessionFork.ExistingEditFork :=
  match i.retainedKind, i.retained with
  | "bound", some binding => .bound binding
  | "unbound", _ => .unbound
  | _, _ => .missing

@[conformance_family]
def editForkDecision : Family EditForkInput SessionFork.EditForkDecision where
  inputs := editForkCases.map fun c =>
    { name := c.name, requested := c.requested
      retainedKind := match c.retained with
        | .missing => "missing" | .unbound => "unbound" | .bound _ => "bound"
      retained := match c.retained with
        | .bound binding => some binding | _ => none }
  model := fun i => SessionFork.decideEditFork i.requested i.retainedFork

structure PayloadRefWire where
  closeId : Nat
  stream : Nat
  deriving ToJson

structure PublicationWire where
  kind : String
  generation : Option Nat
  call : Option Nat
  origin : Option Nat
  deriving ToJson

def PublicationWire.ofModel : CanonicalOutput.MessagePublication → PublicationWire
  | .requestExecution generation =>
      { kind := "request_execution", generation := some generation, call := none, origin := none }
  | .requestRecovery generation =>
      { kind := "request_recovery", generation := some generation, call := none, origin := none }
  | .toolDelivery call => { kind := "tool_delivery", generation := none, call := some call, origin := none }
  | .fork origin => { kind := "fork", generation := none, call := none, origin := some origin }

structure MessageWire where
  docId : Nat
  session : Nat
  messageKey : String
  sequence : Nat
  nativeId : Option String
  nativeRequestId : Option String
  createdAt : Nat
  requestDocId : Option Nat
  originHeaderId : Option Nat
  payloadRefs : List PayloadRefWire
  role : String
  publication : PublicationWire
  deriving ToJson

structure CompactionWire where
  docId : Nat
  session : Nat
  sequence : Nat
  throughSequence : Nat
  deriving ToJson

structure ClosureWire where
  docId : Nat
  session : Nat
  logical : Nat
  deriving ToJson

structure HistoryWire where
  messages : List MessageWire
  compactions : List CompactionWire
  closures : List ClosureWire
  deriving ToJson

def HistoryWire.ofModel (h : SessionFork.History) : HistoryWire :=
  { messages := h.messages.map fun message =>
      { docId := message.header.id, session := message.header.session, messageKey := message.key
        sequence := message.sequence, nativeId := message.nativeId
        nativeRequestId := message.nativeRequestId, createdAt := message.createdAt
        requestDocId := message.header.request, originHeaderId := message.header.origin
        payloadRefs := message.header.refs.map fun ref => { closeId := ref.closeId, stream := ref.stream }
        role := roleName message.header.role
        publication := PublicationWire.ofModel message.header.publication }
    compactions := h.compactions.map fun c =>
      { docId := c.id, session := c.session, sequence := c.sequence, throughSequence := c.throughSequence }
    closures := h.closures.map fun c => { docId := c.id, session := c.session, logical := c.logical } }

/-- Fork inputs and computed durable outputs. Referenced payload closures are
copied into the child under their logical reference; tool rows are never copied
and no reference is remapped. `readers` states which direct session grant reads
the published child's whole transcript. -/
structure ForkCase where
  name : String
  source : SessionFork.History
  target : AgentSession.Scope
  cut : Nat
  authorized : Bool
  idle : Bool
  coherent : Bool

def forkCases : List ForkCase :=
  [ ⟨"exact_copy", history, child, 1, true, true, true⟩,
    ⟨"empty_prefix", history, child, 0, true, true, true⟩,
    ⟨"duplicate_message_sequence", duplicateSequenceHistory, child, 1, true, true, true⟩,
    ⟨"wrong_requester", history, { child with requester := none }, 1, true, true, true⟩,
    ⟨"busy_source", history, child, 1, true, false, true⟩,
    ⟨"mixed_generation", history, child, 1, true, true, false⟩,
    ⟨"unauthorized", history, child, 1, false, true, true⟩,
    ⟨"cursor_beyond_source",
      { history with compactions := [⟨4, scope.session, 1, 99⟩] }, child, 100, true, true, true⟩,
    ⟨"invalid_compaction_sequence",
      { history with compactions := [⟨4, scope.session, 2, 0⟩] }, child, 1, true, true, true⟩ ]

structure ForkInput where
  name : String
  source : HistoryWire
  parent : AgentSession.Scope
  child : AgentSession.Scope
  exclusiveSequenceCut : Nat
  documentIdOffset : Nat
  authorized : Bool
  idle : Bool
  coherent : Bool
  deriving ToJson

def ForkCase.input (c : ForkCase) : ForkInput :=
  { name := c.name, source := HistoryWire.ofModel c.source, parent := scope, child := c.target
    exclusiveSequenceCut := c.cut, documentIdOffset := 1000, authorized := c.authorized
    idle := c.idle, coherent := c.coherent }

def ForkCase.published (c : ForkCase) : Option SessionFork.History :=
  let authorization : SessionFork.SourceAuthorization :=
    ⟨if c.authorized then scope else { scope with node := scope.node + 1 },
      c.source.messages.map (·.header.id), c.source.compactions.map (·.id),
      c.source.closures.map (·.id)⟩
  SessionFork.publish c.source scope c.target remap childKey c.cut authorization c.idle c.coherent

structure ForkReader where
  grant : String
  readsTranscript : Bool
  deriving ToJson

def ForkCase.readers (c : ForkCase) : List ForkReader :=
  (c.published.map fun copied =>
    [("child_session", c.target.session), ("source_session", scope.session),
     ("unrelated_session", c.target.session + scope.session + 1)].map fun (name, grant) =>
      { grant := name, readsTranscript := SessionFork.readsTranscript grant copied c.target.session })
    |>.getD []

def forkCaseOf (i : ForkInput) : Option ForkCase := forkCases.find? (·.name == i.name)

structure ForkReaders where
  readers : List ForkReader
  deriving ToJson

/-- A reader granted only a published fork's own session reads its whole
retained transcript. The served scenario publishes a signed user history and
forks it exactly, so it replays the exact copy. -/
@[conformance_family]
def forkReaders : Family ForkInput ForkReaders where
  inputs := (forkCases.filter fun c => c.name ∈ ["exact_copy"]).map ForkCase.input
  model := fun i => { readers := ((forkCaseOf i).map ForkCase.readers).getD [] }

structure ForkPublication where
  published : Option HistoryWire
  readers : List ForkReader
  deriving ToJson

/-- model-production-gap: production publishes a fork through its served
transaction and assigns DefraDB document identifiers, so no production function
returns the copied history for comparison with `SessionFork.publish`, and no
scenario builds these source histories. -/
@[conformance_family]
def forkPublicationUnreached : Family ForkInput ForkPublication where
  inputs := forkCases.map ForkCase.input
  model := fun i =>
    { published := ((forkCaseOf i).bind ForkCase.published).map HistoryWire.ofModel
      readers := ((forkCaseOf i).map ForkCase.readers).getD [] }

deriving instance ToJson for AgentSession.TaskFireEvidence, AgentSession.StarterEvidence,
  AgentSession.CreationReceipt

structure TaskOpeningInput where
  name : String
  operation : String
  fires : List AgentSession.TaskFireEvidence
  deriving ToJson

structure TaskOpeningOutput where
  task : Option Nat
  deriving ToJson

@[conformance_family]
def taskOpening : Family TaskOpeningInput TaskOpeningOutput where
  inputs :=
    let fire : AgentSession.TaskFireEvidence := ⟨1, 10, 20, 4, true⟩
    ([ ("valid", [fire]), ("missing", []), ("ambiguous", [fire, fire]),
      ("invalid_identity", [{ fire with valid := false }]),
      ("other_node", [{ fire with node := 2 }]),
      ("other_session", [{ fire with session := 11 }]),
      ("other_request", [{ fire with request := 21 }]) ] :
        List (String × List AgentSession.TaskFireEvidence)).map fun (name, fires) =>
      { name, operation := "task_opening", fires }
  model := fun i => { task := AgentSession.taskForOpening 1 10 20 i.fires }

structure ForkAuthorityInput where
  name : String
  operation : String
  sourceAgent : String
  targetAgent : String
  deriving ToJson

structure ForkAuthorityOutput where
  allowed : Bool
  deriving ToJson

@[conformance_family]
def forkReferenceAuthority : Family ForkAuthorityInput ForkAuthorityOutput where
  inputs :=
    [("same_agent", "did:example:one", "did:example:one"),
     ("other_agent", "did:example:one", "did:example:two")].map fun (name, source, target) =>
      { name, operation := "fork_reference_authority", sourceAgent := source, targetAgent := target }
  model := fun i => { allowed := SessionFork.mayReuseSourceFragments i.sourceAgent i.targetAgent }

structure TipCoverageInput where
  operation : String
  complete : Bool
  known : Bool
  materialized : Bool
  observedRequest : Nat
  deriving ToJson

structure TipCoverageOutput where
  pending : Bool
  deriving ToJson

@[conformance_family]
def tipCoverage : Family TipCoverageInput TipCoverageOutput where
  inputs :=
    [(false, true, false, 2), (false, false, false, 1), (false, true, false, 1),
     (false, true, true, 1), (true, true, false, 1), (true, true, true, 1)].map
      fun (complete, known, materialized, observedRequest) =>
        { operation := "tip_coverage", complete, known, materialized, observedRequest }
  model := fun i =>
    { pending := ClientShell.Timeline.pendingRequestOwnerAbsent ⟨i.complete, i.known⟩ 1
        i.observedRequest i.materialized }

structure TimelineMessage where
  key : Nat
  sequence : Int
  role : String
  emitsItem : Bool
  token : Option Nat
  deriving ToJson

structure TimelinePending where
  key : Nat
  beforeSequence : Option Int
  deriving ToJson

structure TimelineOverlay where
  hasDurableOwner : Bool
  beforeSequence : Option Int
  deriving ToJson

structure TimelineInput where
  name : String
  operation : String
  messages : List TimelineMessage
  pending : List TimelinePending
  groups : List Int
  overlay : Option TimelineOverlay
  deriving ToJson

structure TimelineSlot where
  kind : String
  key : Option Nat
  sequence : Option Int
  role : Option String
  deriving ToJson

structure TimelineOutput where
  slots : List TimelineSlot
  deriving ToJson

def timelineRoleName : ClientShell.Timeline.Role → String
  | .user => "user"
  | .assistant => "assistant"

def timelineRole : String → ClientShell.Timeline.Role
  | "user" => .user
  | _ => .assistant

def TimelineSlot.ofModel : ClientShell.Timeline.Slot → TimelineSlot
  | .message key seq role => ⟨"message", some key, some seq, some (timelineRoleName role)⟩
  | .toolGroup seq => ⟨"tool_group", none, some seq, none⟩
  | .pending key => ⟨"pending", some key, none, none⟩
  | .overlay => ⟨"overlay", none, none, none⟩

def timelineInput (name : String) (msgs : List ClientShell.Timeline.Msg)
    (pending : List ClientShell.Timeline.PendingInput)
    (groups : List Int := []) (overlay : Option ClientShell.Timeline.Overlay := none) :
    TimelineInput :=
  { name, operation := "pending_timeline"
    messages := msgs.map fun m =>
      { key := m.key, sequence := m.seq, role := timelineRoleName m.role, emitsItem := m.emitsItem
        token := m.token }
    pending := pending.map fun p =>
      { key := p.key, beforeSequence := match p.placement with
          | .tail => none | .beforeMessage seq => some seq }
    groups
    overlay := overlay.map fun o =>
      { hasDurableOwner := o.hasDurableOwner, beforeSequence := match o.placement with
          | .tail => none | .beforeOrphan seq => some seq } }

@[conformance_family]
def pendingTimeline : Family TimelineInput TimelineOutput where
  inputs :=
    let msgs : List ClientShell.Timeline.Msg := [⟨10, 2, .user, true, none⟩,
      ⟨11, 3, .assistant, true, none⟩]
    [timelineInput "empty_pending_preserves_durable_body" msgs [],
     timelineInput "interrupted_before_next_durable_request" msgs
       [⟨1, .beforeMessage 2⟩],
     timelineInput "equal_anchor_keeps_distinct_request_identities" msgs
       [⟨1, .beforeMessage 2⟩, ⟨2, .beforeMessage 2⟩, ⟨3, .tail⟩],
     timelineInput "first_physical_identity_wins" msgs
       [⟨1, .tail⟩, ⟨1, .beforeMessage 2⟩, ⟨2, .tail⟩],
     timelineInput "missing_anchor_falls_back_before_orphans" msgs
       [⟨1, .beforeMessage 99⟩, ⟨2, .tail⟩] [8],
     timelineInput "tool_first_replica_retains_pending_order"
       [⟨10, 2, .assistant, false, none⟩]
       [⟨1, .beforeMessage 2⟩, ⟨2, .beforeMessage 2⟩] [2, 8],
     timelineInput "nonemitting_anchor_without_group_is_not_visible"
       [⟨10, 2, .assistant, false, none⟩, ⟨11, 3, .assistant, true, none⟩]
       [⟨1, .beforeMessage 2⟩, ⟨2, .tail⟩],
     timelineInput "pending_tail_keeps_overlay_orphan_placement" msgs
       [⟨1, .tail⟩, ⟨2, .tail⟩] [7, 8] (some ⟨false, .beforeOrphan 8⟩),
     timelineInput "durable_owner_suppresses_overlay_only" msgs
       [⟨1, .tail⟩, ⟨2, .tail⟩] [8] (some ⟨true, .tail⟩)]
  model := fun i =>
    { slots := (ClientShell.Timeline.buildOrder i.groups
        (i.messages.map fun m => ⟨m.key, m.sequence, timelineRole m.role, m.emitsItem, m.token⟩)
        (i.pending.map fun p => ⟨p.key, match p.beforeSequence with
          | none => .tail | some seq => .beforeMessage seq⟩)
        (i.overlay.map fun o => ⟨o.hasDurableOwner, match o.beforeSequence with
          | none => .tail | some seq => .beforeOrphan seq⟩)).map TimelineSlot.ofModel }

structure RenameInput where
  name : String
  operation : String
  before : DocumentWire
  now : Nat
  title : Option TitleWire
  deriving ToJson

structure DocumentAfter where
  after : DocumentWire
  deriving ToJson

def renameBefore : AgentSession.Document :=
  { indexed with title := some ⟨"task title", .task⟩ }

@[conformance_family]
def sessionRename : Family RenameInput DocumentAfter where
  inputs :=
    ([("user_rename_preserves_latest", 1, some ⟨"renamed", .user⟩),
      ("user_clear_preserves_latest", 4, none)] : List (String × Nat × Option AgentSession.Title)).map
      fun (name, now, title) =>
        { name, operation := "rename", before := DocumentWire.ofModel renameBefore, now
          title := title.map fun t => { text := t.text, source := t.source.toWireName } }
  model := fun i =>
    { after := DocumentWire.ofModel (AgentSession.rename i.before.toModel
        (i.title.map fun t => ⟨t.text, titleSourceOfWire t.source⟩) i.now) }

structure ToolActivityInput where
  name : String
  operation : String
  before : DocumentWire
  rows : List RequestWire
  event : RequestWire
  now : Nat
  published : Bool
  deriving ToJson

structure ToolActivityStep where
  now : Nat
  after : DocumentWire
  deriving ToJson

structure ToolActivityOutput where
  steps : List ToolActivityStep
  deriving ToJson

def toolActivityInput (name : String) (now : Nat) (published : Bool) (stale : Bool := false) :
    ToolActivityInput :=
  let r := { processingEvent with scope := { scope with requester := none } }
  let before := AgentSession.advance { session with scope := r.scope } [r] r "new" 3
  let event := if stale then { r.observed with docId := 999 } else r.observed
  { name, operation := "tool_activity", before := DocumentWire.ofModel before
    rows := [RequestWire.ofModel { r with observed := event }]
    event := RequestWire.ofModel { r with observed := event }, now, published }

@[conformance_family]
def toolActivity : Family ToolActivityInput ToolActivityOutput where
  inputs :=
    [ toolActivityInput "processing_tool_completion" 4 true,
      toolActivityInput "processing_tool_clock_regression" 2 true,
      toolActivityInput "stale_tool_completion" 4 true true,
      toolActivityInput "unpublished_tool_completion" 4 false ]
  model := fun i =>
    let rows := i.rows.map RequestWire.toModel
    let event := i.event.toModel.observed
    let after := AgentSession.toolActivity i.before.toModel rows event i.now i.published
    { steps :=
        [ { now := i.now, after := DocumentWire.ofModel after },
          { now := i.now + 1
            after := DocumentWire.ofModel
              (AgentSession.toolActivity after rows event (i.now + 1) i.published) } ] }

structure AdvanceInput where
  name : String
  operation : String
  before : DocumentWire
  rows : List RequestWire
  request : RequestWire
  normalizedPreview : String
  now : Nat
  deriving ToJson

def advanceInput (name : String) (r : AgentSession.RequestFact)
    (before : AgentSession.Document := indexed)
    (rows : List AgentSession.RequestFact := [old, newerRequest]) : AdvanceInput :=
  { name, operation := "advance", before := DocumentWire.ofModel before
    rows := rows.map RequestWire.ofModel, request := RequestWire.ofModel r
    normalizedPreview := "candidate", now := 4 }

/-- model-production-gap: the session observation owner advances the public
head inside served request admission transactions; no production function
takes a modeled document and request rows, and no scenario builds these
request histories. -/
@[conformance_family]
def sessionAdvanceUnreached : Family AdvanceInput DocumentAfter where
  inputs :=
    [ advanceInput "stale_admission" old,
      advanceInput "current_admission" newerRequest,
      advanceInput "title_does_not_advance_public_observation" titleRequest session
        [old, newerRequest, titleRequest],
      advanceInput "foreign_requester_does_not_freeze_own_index" scopedSuccessor indexed
        [old, newerRequest, scopedSuccessor, foreignRequester],
      advanceInput "present_requester_does_not_freeze_absent_scope" absentScopeRequest
        absentScopeSession [absentScopeRequest, foreignRequester] ]
  model := fun i =>
    { after := DocumentWire.ofModel (AgentSession.advance i.before.toModel
        (i.rows.map RequestWire.toModel) i.request.toModel i.normalizedPreview i.now) }

structure RefreshInput where
  name : String
  operation : String
  before : DocumentWire
  event : RequestWire
  rows : List RequestWire
  now : Nat
  deriving ToJson

def refreshInput (name : String) (event : AgentSession.RequestFact)
    (rows : List AgentSession.RequestFact := [old, event]) : RefreshInput :=
  { name, operation := "refresh", before := DocumentWire.ofModel indexed
    event := RequestWire.ofModel event, rows := rows.map RequestWire.ofModel, now := 4 }

/-- model-production-gap: the session observation owner refreshes the public
head inside served request transactions; no production function takes a
modeled document and request rows, and no scenario builds these request
histories. -/
@[conformance_family]
def sessionRefreshUnreached : Family RefreshInput DocumentAfter where
  inputs :=
    [ refreshInput "stale_completion" old,
      refreshInput "wrong_physical_completion"
        { newerRequest with observed := { newerRequest.observed with docId := 999 } },
      refreshInput "current_completion" completedRequest,
      refreshInput "stale_event_uses_current_completed_row" processingEvent [completedRequest],
      refreshInput "missing_current_row_noop" newerRequest [],
      refreshInput "foreign_scope_current_row_noop" newerRequest
        [{ completedRequest with scope := { completedRequest.scope with requester := some 99 } }],
      refreshInput "wrong_agent_current_row_noop" newerRequest [{ completedRequest with agent:= 99 }] ]
  model := fun i =>
    { after := DocumentWire.ofModel (AgentSession.refresh i.before.toModel
        (i.rows.map RequestWire.toModel) i.event.toModel.observed i.now) }

structure AdmissionStateWire where
  session : Option DocumentWire
  sessionOwner : Option Nat
  sessionDoc : Option Nat
  sessionParent : Option Nat
  sessionWorker : Option Nat
  receipt : Option AgentSession.CreationReceipt
  requestDoc : Option Nat
  requestOwner : Option Nat
  requestParent : Option Nat
  requestWorker : Option Nat
  deriving ToJson

def AdmissionStateWire.ofModel (s : AgentSession.AdmissionState) : AdmissionStateWire :=
  { session := s.session.map DocumentWire.ofModel, sessionOwner := s.sessionOwner
    sessionDoc := s.sessionDoc, sessionParent := s.sessionParent, sessionWorker := s.sessionWorker
    receipt := s.receipt, requestDoc := s.requestDoc, requestOwner := s.requestOwner
    requestParent := s.requestParent, requestWorker := s.requestWorker }

/-- The wire projection of a modeled atomic admission case. The model reads
the case's own state and input by name. -/
structure AdmissionCaseInput where
  name : String
  operation : String
  before : AdmissionStateWire
  requested : DocumentWire
  actor : Nat
  sessionDoc : Nat
  agentDoc : Nat
  requestDoc : Nat
  receipt : Option AgentSession.CreationReceipt
  authorized : Bool
  commit : Bool
  deriving ToJson

def admissionInputs : List AdmissionCaseInput :=
  admissionCases.map fun (name, before, input) =>
    { name, operation := "atomic_admission", before := AdmissionStateWire.ofModel before
      requested := DocumentWire.ofModel input.requested, actor := input.actor
      sessionDoc := input.sessionDoc, agentDoc := input.agentDoc, requestDoc := input.requestDoc
      receipt := input.receipt, authorized := input.authorized, commit := input.commit }

def admissionAfter (i : AdmissionCaseInput) : AgentSession.AdmissionState :=
  ((admissionCases.find? (·.1 == i.name)).map fun (_, before, input) =>
    AgentSession.admitRequest before input).getD {}

structure AdmissionTitle where
  title : Option TitleWire
  deriving ToJson

/-- The task delivery scenario admits an unattended Agent-created session on a
served node and reads the created session's title. -/
@[conformance_family]
def atomicAdmissionUnattended : Family AdmissionCaseInput AdmissionTitle where
  inputs := admissionInputs.filter fun i => i.name ∈ ["agent_unattended_creation"]
  model := fun i => { title := ((admissionAfter i).session.bind (·.title)).map fun t =>
    { text := t.text, source := t.source.toWireName } }

structure AdmissionAfter where
  after : AdmissionStateWire
  deriving ToJson

/-- model-production-gap: atomic admission runs inside the served request
admission transaction with DefraDB identities and ACP relationships; no
production function returns the modeled admission state, and only the
unattended creation's session title is observed by a scenario. -/
@[conformance_family]
def atomicAdmissionUnreached : Family AdmissionCaseInput AdmissionAfter where
  inputs := admissionInputs
  model := fun i => { after := AdmissionStateWire.ofModel (admissionAfter i) }

end Conformance.Contracts
