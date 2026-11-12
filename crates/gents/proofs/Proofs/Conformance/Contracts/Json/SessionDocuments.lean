import Proofs.Conformance.TaskLineage
import Proofs.Clients.ClientShell.Timeline
import Proofs.Conformance.QuestionReply
import Proofs.Session.SessionQueue.Archive
import Proofs.Conformance.ContractCases.SessionDocuments
import Proofs.Conformance.ContractCases.Types
import Lean
import Proofs.Conformance.RuntimeInput
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.Contracts
open Lean Conformance.SessionDocuments

deriving instance ToJson for AgentSession.Scope

def pluginJobOpeningCasesJson : String :=
  let good : AgentSession.PluginJobOpening := ⟨true, true, true, true, true⟩
  let cases : List (String × AgentSession.PluginJobOpening) :=
    [("valid", good), ("wrong_pin", { good with pinnedSpawnMatches := false }),
     ("failed_transaction", { good with writesComplete := false })]
  (toJson (cases.map fun (name, opening) => Json.mkObj [
    ("name", toJson name), ("commits", toJson (AgentSession.pluginJobOpeningCommits opening))])).compress

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

private def admissionReceiptJson (receipt : AgentSession.CreationReceipt) : Json := Json.mkObj
  [("scope", scopeJson receipt.scope), ("session_doc", toJson receipt.sessionDoc),
   ("agent", toJson receipt.agent), ("opening_request_doc", toJson receipt.openingRequestDoc),
   ("causal_kind", toJson receipt.causalKind), ("cause_binding", toJson receipt.causeBinding),
   ("starter", receipt.starter.map (fun starter => Json.mkObj
      [("source", scopeJson starter.source), ("parent_request_doc", toJson starter.parentRequestDoc)])
      |>.getD Json.null)]

private def admissionStateJson (state : AgentSession.AdmissionState) : Json := Json.mkObj
  [("session", state.session.map documentJson |>.getD Json.null),
   ("session_owner", toJson state.sessionOwner), ("session_doc", toJson state.sessionDoc),
   ("session_parent", toJson state.sessionParent), ("session_worker", toJson state.sessionWorker),
   ("receipt", state.receipt.map admissionReceiptJson |>.getD Json.null),
   ("request_doc", toJson state.requestDoc), ("request_owner", toJson state.requestOwner),
   ("request_parent", toJson state.requestParent), ("request_worker", toJson state.requestWorker)]

private def admissionJson (name : String) (before : AgentSession.AdmissionState)
    (input : AgentSession.AdmissionInput) : Json := Json.mkObj
  [("name", toJson name), ("operation", toJson "atomic_admission"),
   ("before", admissionStateJson before), ("requested", documentJson input.requested),
   ("actor", toJson input.actor), ("session_doc", toJson input.sessionDoc),
   ("agent_doc", toJson input.agentDoc), ("request_doc", toJson input.requestDoc),
   ("receipt", input.receipt.map admissionReceiptJson |>.getD Json.null),
   ("authorized", toJson input.authorized), ("commit", toJson input.commit),
   ("after", admissionStateJson (AgentSession.admitRequest before input))]

private def advanceJson (name : String) (r : AgentSession.RequestFact)
    (before : AgentSession.Document := indexed)
    (rows : List AgentSession.RequestFact := [old, newerRequest]) : Json := Json.mkObj
  [("name", toJson name), ("operation", toJson "advance"), ("before", documentJson before),
   ("rows", toJson (rows.map requestJson)), ("request", requestJson r),
   ("normalized_preview", toJson "candidate"), ("now", toJson (4 : Nat)),
   ("after", documentJson (AgentSession.advance before rows r "candidate" 4))]
private def refreshJson (name : String) (event : AgentSession.RequestFact)
    (rows : List AgentSession.RequestFact := [old, event]) : Json := Json.mkObj
  [("name", toJson name), ("operation", toJson "refresh"), ("before", documentJson indexed),
   ("event", requestJson event), ("rows", toJson (rows.map requestJson)), ("now", toJson (4 : Nat)),
   ("after", documentJson (AgentSession.refresh indexed rows event.observed 4))]
private def toolActivityJson (name : String) (now : Nat) (published : Bool)
    (stale : Bool := false) : Json :=
  let r := { processingEvent with scope := { scope with requester := none } }
  let before := AgentSession.advance { session with scope := r.scope } [r] r "new" 3
  let event := if stale then { r.observed with docId := 999 } else r.observed
  let rows := [{ r with observed := event }]
  let after := AgentSession.toolActivity before rows event now published
  Json.mkObj [("name", toJson name), ("operation", toJson "tool_activity"),
    ("before", documentJson before), ("rows", toJson (rows.map requestJson)),
    ("event", requestJson { r with observed := event }), ("now", toJson now),
    ("published", toJson published),
    ("steps", toJson [
      Json.mkObj [("now", toJson now), ("after", documentJson after)],
      Json.mkObj [("now", toJson (now + 1)),
        ("after", documentJson (AgentSession.toolActivity after rows event (now + 1) published))]])]
private def tipCoverageJson (complete known materialized : Bool) (observed : Nat := 1) : Json :=
  let coverage : ClientShell.Timeline.ReadCoverage := ⟨complete, known⟩
  Json.mkObj [("operation", toJson "tip_coverage"),
    ("complete", toJson complete), ("known", toJson known),
    ("materialized", toJson materialized), ("observed_request", toJson observed),
    ("pending", toJson (ClientShell.Timeline.pendingRequestOwnerAbsent coverage 1 observed materialized))]
private def timelineSlotJson : ClientShell.Timeline.Slot → Json
  | .message key seq role => Json.mkObj [("kind", toJson "message"),
      ("key", toJson key), ("sequence", toJson seq),
      ("role", toJson (match role with | .user => "user" | .assistant => "assistant"))]
  | .toolGroup seq => Json.mkObj [("kind", toJson "tool_group"), ("sequence", toJson seq)]
  | .pending key => Json.mkObj [("kind", toJson "pending"), ("key", toJson key)]
  | .overlay => Json.mkObj [("kind", toJson "overlay")]

private def pendingTimelineJson (name : String) (msgs : List ClientShell.Timeline.Msg)
    (pending : List ClientShell.Timeline.PendingInput)
    (groups : List Int := []) (overlay : Option ClientShell.Timeline.Overlay := none) : Json :=
  Json.mkObj [("name", toJson name), ("operation", toJson "pending_timeline"),
    ("messages", toJson (msgs.map (fun m => Json.mkObj [
      ("key", toJson m.key), ("sequence", toJson m.seq),
      ("role", toJson (match m.role with | .user => "user" | .assistant => "assistant")),
      ("emits_item", toJson m.emitsItem), ("token", toJson m.token)]))),
    ("pending", toJson (pending.map (fun p => Json.mkObj [
      ("key", toJson p.key), ("before_sequence", toJson (match p.placement with
        | .tail => (none : Option Int) | .beforeMessage seq => some seq))]))),
    ("groups", toJson groups),
    ("overlay", overlay.map (fun o => Json.mkObj [
      ("has_durable_owner", toJson o.hasDurableOwner),
      ("before_sequence", toJson (match o.placement with
        | .tail => (none : Option Int) | .beforeOrphan seq => some seq))]) |>.getD Json.null),
    ("slots", toJson ((ClientShell.Timeline.buildOrder groups msgs pending overlay).map timelineSlotJson))]

private def pendingTimelineCases : List Json :=
  let msgs : List ClientShell.Timeline.Msg := [⟨10, 2, .user, true, none⟩,
    ⟨11, 3, .assistant, true, none⟩]
  [pendingTimelineJson "empty_pending_preserves_durable_body" msgs [],
   pendingTimelineJson "interrupted_before_next_durable_request" msgs
     [⟨1, .beforeMessage 2⟩],
   pendingTimelineJson "equal_anchor_keeps_distinct_request_identities" msgs
     [⟨1, .beforeMessage 2⟩, ⟨2, .beforeMessage 2⟩, ⟨3, .tail⟩],
   pendingTimelineJson "first_physical_identity_wins" msgs
     [⟨1, .tail⟩, ⟨1, .beforeMessage 2⟩, ⟨2, .tail⟩],
   pendingTimelineJson "missing_anchor_falls_back_before_orphans" msgs
     [⟨1, .beforeMessage 99⟩, ⟨2, .tail⟩] [8],
   pendingTimelineJson "tool_first_replica_retains_pending_order"
     [⟨10, 2, .assistant, false, none⟩]
     [⟨1, .beforeMessage 2⟩, ⟨2, .beforeMessage 2⟩] [2, 8],
   pendingTimelineJson "nonemitting_anchor_without_group_is_not_visible"
     [⟨10, 2, .assistant, false, none⟩, ⟨11, 3, .assistant, true, none⟩]
     [⟨1, .beforeMessage 2⟩, ⟨2, .tail⟩],
   pendingTimelineJson "pending_tail_keeps_overlay_orphan_placement" msgs
     [⟨1, .tail⟩, ⟨2, .tail⟩] [7, 8] (some ⟨false, .beforeOrphan 8⟩),
   pendingTimelineJson "durable_owner_suppresses_overlay_only" msgs
     [⟨1, .tail⟩, ⟨2, .tail⟩] [8] (some ⟨true, .tail⟩)] ++ Conformance.QuestionReply.cases

private def renameBefore : AgentSession.Document :=
  { indexed with title := some ⟨"task title", .task⟩ }
private def renameJson : Json := Json.mkObj
  [("name", toJson "user_rename_preserves_latest"), ("operation", toJson "rename"),
   ("before", documentJson renameBefore), ("now", toJson (1 : Nat)),
   ("title", Json.mkObj [("text", toJson "renamed"), ("source", toJson "user")]),
   ("after", documentJson (AgentSession.rename renameBefore (some ⟨"renamed", .user⟩) 1))]
private def clearTitleJson : Json := Json.mkObj
  [("name", toJson "user_clear_preserves_latest"), ("operation", toJson "rename"),
   ("before", documentJson renameBefore), ("now", toJson (4 : Nat)),
   ("title", Json.null),
   ("after", documentJson (AgentSession.rename renameBefore none 4))]
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

private def taskOpeningCases : List Json :=
  let fire : AgentSession.TaskFireEvidence := ⟨1, 10, 20, 4, true⟩
  [ ("valid", [fire]), ("missing", []), ("ambiguous", [fire, fire]),
    ("invalid_identity", [{ fire with valid := false }]),
    ("other_node", [{ fire with node := 2 }]),
    ("other_session", [{ fire with session := 11 }]),
    ("other_request", [{ fire with request := 21 }]) ].map fun (name, fires) =>
      Json.mkObj [("name", toJson name), ("operation", toJson "task_opening"),
        ("fires", toJson (fires.map fun f => Json.mkObj [("node", toJson f.node),
          ("session", toJson f.session), ("request", toJson f.request),
          ("task", toJson f.task), ("valid", toJson f.valid)])),
        ("task", toJson (AgentSession.taskForOpening 1 10 20 fires))]

private def forkAuthorityCases : List Json :=
  [("same_agent", "did:example:one", "did:example:one"),
   ("other_agent", "did:example:one", "did:example:two")].map fun (name, source, target) =>
    Json.mkObj [("name", toJson name), ("operation", toJson "fork_reference_authority"),
      ("source_agent", toJson source), ("target_agent", toJson target),
      ("allowed", toJson (SessionFork.mayReuseSourceFragments source target))]

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

def sessionDocumentProjectionCasesJson : String :=
  (toJson (Conformance.TaskLineage.cases ++ taskOpeningCases ++ forkAuthorityCases ++ pendingTimelineCases ++
      (admissionCases.map fun (name, before, input) => admissionJson name before input) ++ [renameJson, clearTitleJson,
      tipCoverageJson false true false 2,
      tipCoverageJson false false false,
      tipCoverageJson false true false,
      tipCoverageJson false true true,
      tipCoverageJson true true false,
      tipCoverageJson true true true,
      toolActivityJson "processing_tool_completion" 4 true,
      toolActivityJson "processing_tool_clock_regression" 2 true,
      toolActivityJson "stale_tool_completion" 4 true true,
      toolActivityJson "unpublished_tool_completion" 4 false,
      advanceJson "stale_admission" old,
      advanceJson "current_admission" newerRequest,
      advanceJson "title_does_not_advance_public_observation" titleRequest session
        [old, newerRequest, titleRequest],
      advanceJson "foreign_requester_does_not_freeze_own_index" scopedSuccessor indexed
        [old, newerRequest, scopedSuccessor, foreignRequester],
      advanceJson "present_requester_does_not_freeze_absent_scope" absentScopeRequest absentScopeSession
        [absentScopeRequest, foreignRequester],
      refreshJson "stale_completion" old,
      refreshJson "wrong_physical_completion"
        { newerRequest with observed := { newerRequest.observed with docId := 999 } },
      refreshJson "current_completion" completedRequest,
      refreshJson "stale_event_uses_current_completed_row" processingEvent [completedRequest],
      refreshJson "missing_current_row_noop" newerRequest [],
      refreshJson "foreign_scope_current_row_noop" newerRequest
        [{ completedRequest with scope := { completedRequest.scope with requester := some 99 } }],
      refreshJson "wrong_agent_current_row_noop" newerRequest [{ completedRequest with agent:= 99 }]] ++ Conformance.RuntimeInput.cases)).compress

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

end Conformance.Contracts
