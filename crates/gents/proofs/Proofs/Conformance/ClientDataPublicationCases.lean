import Proofs.Clients.ClientShell.ObservationOrdering
import Proofs.Conformance.Family

namespace Conformance.ClientDataPublication

open Lean _root_.ClientDataPublication

deriving instance ToJson for Recovery, ClientStoreNotice.Notice

inductive PublicationStep where
  | requestEvent
  | transcriptInvalidation
  | replaceRequests
  deriving ToJson

structure PublicationInput where
  name : String
  actions : List PublicationStep
  initialRequestIds : List String
  publishRequestIds : List String
  deriving ToJson

structure PublicationOutput where
  accepted : Bool
  deriving ToJson

def publicationStart : State :=
  { dataEpoch := 4, provenanceEpoch := 7, requestEventEpoch := 3, transcriptEpoch := 2 }

def applyPublicationStep (read : Read) (state : State) : PublicationStep → State
  | .requestEvent => requestEvent state
  | .transcriptInvalidation => transcriptInvalidation state
  | .replaceRequests => publish state read

@[conformance_family]
def publication : Family PublicationInput PublicationOutput where
  inputs :=
    [ ("uncached_request_event_fences_old_read", PublicationStep.requestEvent),
      ("transcript_invalidation_preserves_read", .transcriptInvalidation),
      ("newer_data_publication_rejects_old_read", .replaceRequests) ].map fun (name, step) =>
      { name, actions := [step], initialRequestIds := ["old"], publishRequestIds := ["old"] }
  model := fun i =>
    let read := beginRead publicationStart
    { accepted := accepts (i.actions.foldl (applyPublicationStep read) publicationStart) read }

inductive NodeOp where
  | merge
  | request
  | transcript
  deriving ToJson

structure NodeAction where
  op : NodeOp
  node : Nat
  deriving ToJson

structure NodePublicationInput where
  name : String
  merge : Bool
  actions : List NodeAction
  deriving ToJson

structure NodeRow where
  node : Nat
  value : String
  deriving ToJson

structure NodePublicationOutput where
  accepted : Bool
  expected : List NodeRow
  deriving ToJson

def nodeStart : State :=
  { dataEpoch := 1, provenanceEpoch := 1, requestEventEpoch := 1, transcriptEpoch := 0 }

def applyNodeAction (state : Fleet × (Nat → String)) (action : NodeAction) :
    Fleet × (Nat → String) :=
  match action.op with
  | .merge =>
      (state.1.on action.node dataChange,
        fun other => if other = action.node then "concurrent" else state.2 other)
  | .request => (state.1.on action.node requestEvent, state.2)
  | .transcript => (state.1.on action.node transcriptInvalidation, state.2)

/-- Two nodes, each with its own store holding one session row. A read of node
1 publishes `incoming` as a patch (`merge`) or a reload after `actions`. -/
@[conformance_family]
def nodePublication : Family NodePublicationInput NodePublicationOutput where
  inputs :=
    [ ("other_node_publication_preserves_reload", false, [⟨.merge, 2⟩]),
      ("continuous_other_node_publications_preserve_reload", false, List.replicate 64 ⟨.merge, 2⟩),
      ("same_node_publication_rejects_reload", false, [⟨.merge, 1⟩]),
      ("same_node_request_event_preserves_reload", false, [⟨.request, 1⟩]),
      ("sustained_request_events_preserve_reload", false, List.replicate 64 ⟨.request, 1⟩),
      ("transcript_notice_preserves_reload", false, [⟨.transcript, 1⟩]),
      ("same_node_request_event_rejects_patch", true, [⟨.request, 1⟩]),
      ("same_node_publication_rejects_patch", true, [⟨.merge, 1⟩]),
      ("other_node_request_event_preserves_patch", true, [⟨.request, 2⟩]),
      ("other_node_publication_preserves_patch", true, [⟨.merge, 2⟩]) ].map
      fun (name, merge, actions) => { name, merge, actions }
  model := fun i =>
    let fleet : Fleet := fun _ => nodeStart
    let read := beginRead (fleet 1)
    let (current, rows) := i.actions.foldl applyNodeAction (fleet, fun _ => "initial")
    let accepted := if i.merge then accepts (current 1) read else acceptsReload (current 1) read
    { accepted
      expected := [1, 2].map fun node =>
        { node, value := if node = 1 ∧ accepted = true then "incoming" else rows node } }

inductive RecoveryOp where
  | requestScope
  | takeBatch
  | failBatch
  | retryTimer
  deriving ToJson

structure RecoveryAction where
  op : RecoveryOp
  scope : Option Nat
  scopes : List Nat
  deriving ToJson

structure RecoveryInput where
  name : String
  actions : List RecoveryAction
  deriving ToJson

structure RecoveryOutput where
  initial : Recovery
  pendingScopes : List Nat
  generation : Nat
  retryScheduled : Bool
  deriving ToJson

def recoveryStart : Recovery :=
  { pendingScopes := [], generation := 0, retryScheduled := false }

def applyRecoveryAction (recovery : Recovery) (action : RecoveryAction) : Recovery :=
  match action.op, action.scope with
  | .requestScope, some scope => requestScope recovery scope
  | .requestScope, none => recovery
  | .takeBatch, _ => (takeBatch recovery).1
  | .failBatch, _ => failBatch recovery action.scopes
  | .retryTimer, _ => retryTimer recovery

def recoveryActions (setupScopes failedScopes externalScopes : List Nat)
    (timerAdvanced : Bool) : List RecoveryAction :=
  setupScopes.map (fun scope => ⟨.requestScope, some scope, []⟩) ++
    [⟨.takeBatch, none, []⟩, ⟨.failBatch, none, failedScopes⟩] ++
    externalScopes.map (fun scope => ⟨.requestScope, some scope, []⟩) ++
    (if timerAdvanced then [⟨.retryTimer, none, []⟩] else [])

@[conformance_family]
def recovery : Family RecoveryInput RecoveryOutput where
  inputs :=
    [ ("failed_batch_retains_failed_and_unprocessed_scopes", recoveryActions [1, 2, 3] [1, 2, 3] [] false),
      ("new_scope_during_retry_is_retained", recoveryActions [1, 2, 3] [1, 2, 3] [4] false),
      ("retry_timer_releases_gate_without_losing_scopes", recoveryActions [1, 2, 3] [1, 2, 3] [] true) ].map
      fun (name, actions) => { name, actions }
  model := fun i =>
    let final := i.actions.foldl applyRecoveryAction recoveryStart
    { initial := recoveryStart
      pendingScopes := final.pendingScopes
      generation := final.generation
      retryScheduled := final.retryScheduled }

structure NoticeTrace where
  initial : ClientStoreNotice.Notice
  deliveries : List ClientStoreNotice.Notice
  deriving ToJson

structure Delivered where
  change : ClientStoreNotice.Notice
  watchedVersion : Nat
  deriving ToJson

@[conformance_family]
def noticeDelivery : Family NoticeTrace (List Delivered) where
  inputs :=
    [ { initial := ⟨1, 1, 0⟩
        deliveries := [⟨3, 2, 1⟩, ⟨2, 1, 1⟩, ⟨3, 2, 1⟩, ⟨5, 3, 2⟩, ⟨4, 2, 2⟩] } ]
  model := fun t =>
    (t.deliveries.foldl (fun (current, delivered) incoming =>
      let next := ClientStoreNotice.deliver current incoming
      (next, delivered ++ [{ change := next, watchedVersion := next.version }]))
      (t.initial, ([] : List Delivered))).2

end Conformance.ClientDataPublication
