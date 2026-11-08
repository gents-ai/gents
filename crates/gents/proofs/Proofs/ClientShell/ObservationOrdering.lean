import Proofs.ClientShell.Types

/-!
Ephemeral async presentation fence. These epochs order a single UI owner's
selection intent, not documents, replication, identity, or request lifecycle.
A successful mutation still exists even when its late presentation is discarded.
-/
namespace ClientObservationOrdering

variable {α : Type}

def accepts (current captured : Nat) : Bool := decide (current = captured)

structure View (α : Type) where
  epoch : Nat
  value : α

def select (s : View α) (value : α) : View α :=
  { epoch := s.epoch + 1, value }

def finish (s : View α) (captured : Nat) (result : α) : View α :=
  if accepts s.epoch captured then { s with value := result } else s

theorem unchanged_intent_accepts (s : View α) (result : α) :
    (finish s s.epoch result).value = result := by
  simp [finish, accepts]

theorem changed_intent_ignores_completion (s : View α) (selected result : α) :
    finish (select s selected) s.epoch result = select s selected := by
  simp [finish, select, accepts]

theorem navigate_away_and_back_ignores_completion (s : View α) (other result : α) :
    finish (select (select s other) s.value) s.epoch result =
      select (select s other) s.value := by
  have hne : s.epoch + 1 + 1 ≠ s.epoch := by omega
  simp [finish, select, accepts, hne]

theorem older_epoch_cannot_commit (s : View α) (captured : Nat) (result : α)
    (h : captured < s.epoch) : finish s captured result = s := by
  have hne : s.epoch ≠ captured := by omega
  simp [finish, accepts, hne]

theorem completion_preserves_epoch (s : View α) (captured : Nat) (result : α) :
    (finish s captured result).epoch = s.epoch := by
  simp only [finish]
  split <;> rfl

def releaseOwned (current : Option Nat) (owned : Nat) : Option Nat :=
  if current = some owned then none else current

theorem own_pending_marker_released (owned : Nat) :
    releaseOwned (some owned) owned = none := by simp [releaseOwned]

theorem other_pending_marker_preserved (current : Option Nat) (owned : Nat)
    (h : current ≠ some owned) : releaseOwned current owned = current := by
  simp [releaseOwned, h]

end ClientObservationOrdering

/-!
Accepted submission cleanup is scoped to the draft context that originated the
mutation. Navigation does not revoke cleanup of the submitted text, while an
edit in that origin context remains authoritative.
-/
namespace ClientDraftOwnership

variable {Key Text : Type} [DecidableEq Key] [DecidableEq Text]

def clearAcceptedOrigin
    (drafts : Key → Option Text) (origin : Key) (submitted : Text) :
    Key → Option Text :=
  fun key =>
    if key = origin ∧ drafts origin = some submitted then none else drafts key

theorem accepted_origin_text_clears
    (drafts : Key → Option Text) (origin : Key) (submitted : Text)
    (h : drafts origin = some submitted) :
    clearAcceptedOrigin drafts origin submitted origin = none := by
  simp [clearAcceptedOrigin, h]

theorem edited_origin_text_is_preserved
    (drafts : Key → Option Text) (origin : Key) (submitted edited : Text)
    (hDraft : drafts origin = some edited) (hEdit : edited ≠ submitted) :
    clearAcceptedOrigin drafts origin submitted origin = some edited := by
  simp [clearAcceptedOrigin, hDraft, hEdit]

theorem unrelated_draft_is_preserved
    (drafts : Key → Option Text) (origin other : Key) (submitted : Text)
    (h : other ≠ origin) :
    clearAcceptedOrigin drafts origin submitted other = drafts other := by
  simp [clearAcceptedOrigin, h]

end ClientDraftOwnership

namespace ClientSnapshotObservation

open ClientObservationOrdering

variable {α : Type}

/-- Only issuing a read advances observation order. Mutation payloads are not
published: successful mutations request a fresh read; failures leave it alone. -/
def beginRead (s : View α) : View α := { s with epoch := s.epoch + 1 }

def mutationCompleted (s : View α) (succeeded : Bool) : View α :=
  if succeeded then beginRead s else s

theorem failed_mutation_preserves_pending_read (s : View α) (result : α) :
    finish (mutationCompleted s false) s.epoch result = { s with value := result } := by
  simp [mutationCompleted, finish, accepts]

theorem mutation_refresh_rejects_precompletion_read (s : View α) (stale : α) :
    finish (mutationCompleted s true) s.epoch stale = mutationCompleted s true := by
  simp [mutationCompleted, beginRead, finish, accepts]

theorem fresh_read_after_mutation_replaces_intermediate_state
    (s : View α) (intermediate authoritative : α) :
    let observed := finish s s.epoch intermediate
    let refresh := mutationCompleted observed true
    (finish refresh refresh.epoch authoritative).value = authoritative := by
  simp [finish, accepts]

inductive StartupPhase where
  | checkingManagedServer | loadingConfiguration | startingClient
  | managedServerError | configurationError | clientError | ready
  deriving DecidableEq, Repr

inductive FirstRunPhase where
  | unknown | active | done
  deriving DecidableEq, Repr

inductive InitConfigStatus where
  | absent | configured | unreadable | unknown
  deriving DecidableEq, Repr

/-- Remote-only clients have no local init file, and a desktop that may run a
local node has none until local setup is chosen. In either case a persisted
enrollment marks configuration complete even while the live route reconnects
after restart; local setup stays available from the node list. Enrollment
authority and send readiness remain with their existing owners; operator
configuration access is not required. Unreadable bootstrap state stays
unresolved, and an active wizard retains ownership until explicit completion. -/
def observeFirstRun (phase : FirstRunPhase) (snapshotReady : Bool)
    (initConfigStatus : InitConfigStatus)
    (needsSetup authoritative localRuntimeAllowed remoteConfigured : Bool) : FirstRunPhase :=
  match phase with
  | .active => .active
  | .done => .done
  | .unknown =>
      if !snapshotReady then .unknown
      else match initConfigStatus with
        | .unreadable | .unknown => .unknown
        | status =>
          if !localRuntimeAllowed || status == .absent then
            if remoteConfigured then .done else .active
          else if !authoritative then .unknown
          else if needsSetup then .active else .done

theorem configured_remote_completes_without_operator_access :
    observeFirstRun .unknown true .absent true false false true = .done := by rfl

theorem unconfigured_remote_keeps_setup :
    observeFirstRun .unknown true .absent false true false false = .active := by rfl

theorem desktop_saved_remote_completes_without_local_home (needsSetup authoritative : Bool) :
    observeFirstRun .unknown true .absent needsSetup authoritative true true = .done := by
  cases needsSetup <;> cases authoritative <;> rfl

theorem desktop_without_local_home_or_remote_keeps_setup (needsSetup authoritative : Bool) :
    observeFirstRun .unknown true .absent needsSetup authoritative true false = .active := by
  cases needsSetup <;> cases authoritative <;> rfl

theorem active_setup_keeps_ownership (snapshotReady : Bool) (status : InitConfigStatus)
    (needsSetup authoritative localRuntimeAllowed remoteConfigured : Bool) :
    observeFirstRun .active snapshotReady status needsSetup authoritative localRuntimeAllowed remoteConfigured = .active := by
  rfl

theorem unreadable_bootstrap_stays_unresolved
    (needsSetup authoritative localRuntimeAllowed remoteConfigured : Bool) :
    observeFirstRun .unknown true .unreadable needsSetup authoritative localRuntimeAllowed remoteConfigured = .unknown := by
  rfl

/-- A client observation can recover a client error, not managed-server authority.

`autostartDeclined` is true exactly when the desktop will not autostart the
client. The shell derives it from the one predicate that also gates autostart
(`shouldAutoStartDesktopClient`), so startup never waits on a client that
nothing will start. -/
def observeStartup (phase : StartupPhase) (running autostartDeclined : Bool) :
    StartupPhase :=
  match phase with
  | .loadingConfiguration | .startingClient =>
      if running || autostartDeclined then .ready else .startingClient
  | .clientError => if running then .ready else .clientError
  | other => other

theorem observed_running_client_recovers_client_error (autostartDeclined : Bool) :
    observeStartup .clientError true autostartDeclined = .ready := by rfl

theorem stopped_client_does_not_clear_client_error (autostartDeclined : Bool) :
    observeStartup .clientError false autostartDeclined = .clientError := by rfl

theorem client_observation_does_not_clear_managed_error (running autostartDeclined : Bool) :
    observeStartup .managedServerError running autostartDeclined = .managedServerError := by rfl

end ClientSnapshotObservation

/-! Publication tracks the scopes a read may replace independently of UI intent
and lineage provenance. Unclassified changes invalidate every scope. Classified
changes invalidate their canonical node owners; publication rebases onto the
current store so an unrelated update is retained rather than overwritten. -/
namespace ClientDataPublication

inductive Scope where
  | all
  | nodes (owners : List Nat)
  deriving DecidableEq, Repr

structure State where
  dataEpoch : Nat
  provenanceEpoch : Nat
  requestEventEpoch : Nat
  transcriptEpoch : Nat
  sharedEpoch : Nat := 0
  nodeEpoch : Nat → Nat := fun _ => 0
  eventSharedEpoch : Nat := 0
  eventNodeEpoch : Nat → Nat := fun _ => 0

/-- Runtime documents are read from the selected runtime's authenticated database
endpoint under the client DID. The observer arms its GraphQL subscriptions before
requesting the initial snapshot; reconnect, renewal and gaps require a fresh snapshot.
These epochs reject stale publications from those reads. A local replica is never
an alternative source for runtime configuration, sessions or transcript data.
Native subscription delivery may omit permission-denied empty results; document
policy revocation invalidation remains an upstream limitation, not an access grant. -/
structure Read where
  dataEpoch : Nat
  requestEventEpoch : Nat
  sharedEpoch : Nat := 0
  nodeEpoch : Nat → Nat := fun _ => 0
  eventSharedEpoch : Nat := 0
  eventNodeEpoch : Nat → Nat := fun _ => 0

def beginRead (s : State) : Read :=
  { dataEpoch := s.dataEpoch, requestEventEpoch := s.requestEventEpoch,
    sharedEpoch := s.sharedEpoch, nodeEpoch := s.nodeEpoch,
    eventSharedEpoch := s.eventSharedEpoch, eventNodeEpoch := s.eventNodeEpoch }

/-- The footprint includes every owner whose cached facts may change. Missing
ownership, dropped notifications, and unretained lineage require `all`; an
empty footprint has no node-local effect. -/
def touch (s : State) (scope : Scope) : State :=
  match scope with
  | .all => { s with sharedEpoch := s.sharedEpoch + 1 }
  | .nodes owners =>
      { s with nodeEpoch := fun node => s.nodeEpoch node + if node ∈ owners then 1 else 0 }

/-- A request event moves its footprint on the request-event epochs only. -/
def touchEvents (s : State) (scope : Scope) : State :=
  match scope with
  | .all => { s with eventSharedEpoch := s.eventSharedEpoch + 1 }
  | .nodes owners =>
      { s with eventNodeEpoch := fun node =>
          s.eventNodeEpoch node + if node ∈ owners then 1 else 0 }

/-- A snapshot reload replaces its scope with the runtime's rows as of its read.
A request event publishes no rows, so it cannot make the reload older than what
the store shows; the event's own resync request reads again after it. Only a
publication into the same scope since the capture can, so reloads check the
publication epochs alone. Without this, request events arriving at least once
per load reject every reload of the node and its rows never catch up
(`ObserverResync` TLA, `SustainedEvents` and `CrossNode`). -/
def acceptsReloadFor (s : State) (read : Read) (scope : Scope) : Bool :=
  match scope with
  | .all => decide (s.dataEpoch = read.dataEpoch)
  | .nodes owners =>
      decide (s.sharedEpoch = read.sharedEpoch) &&
        owners.all (fun node => s.nodeEpoch node == read.nodeEpoch node)

/-- A patch merges rows into the store, so a row it read before a deletion or
change it was told about would survive the event. Patches also check the
request-event epochs of their footprint. -/
def eventsAcceptFor (s : State) (read : Read) (scope : Scope) : Bool :=
  match scope with
  | .all => decide (s.requestEventEpoch = read.requestEventEpoch)
  | .nodes owners =>
      decide (s.eventSharedEpoch = read.eventSharedEpoch) &&
        owners.all (fun node => s.eventNodeEpoch node == read.eventNodeEpoch node)

def acceptsFor (s : State) (read : Read) (scope : Scope) : Bool :=
  acceptsReloadFor s read scope && eventsAcceptFor s read scope

def accepts (s : State) (read : Read) : Bool :=
  acceptsFor s read .all

def dataChange (s : State) (scope : Scope) : State :=
  { touch s scope with dataEpoch := s.dataEpoch + 1 }

def publishFor (s : State) (read : Read) (scope : Scope) : State :=
  if acceptsFor s read scope then dataChange s scope else s

def publish (s : State) (read : Read) : State :=
  publishFor s read .all

def reloadFor (s : State) (read : Read) (scope : Scope) : State :=
  if acceptsReloadFor s read scope then dataChange s scope else s

def requestEventFor (s : State) (scope : Scope) : State :=
  { touchEvents s scope with requestEventEpoch := s.requestEventEpoch + 1 }

def requestEvent (s : State) : State :=
  requestEventFor s .all

/-- A delivered change on `source`'s subscription. Every change on a runtime,
including one to a row of another node, arrives as an event of that runtime's
node. Its footprint is that node because the desktop takes no other node's
rows from that runtime: a reload of `source` publishes only `publishedRows
source`, and another node's rows are read only from that node's runtime. An
event without its source (a reconnect) stays unclassified (`requestEvent`). -/
def observerEvent (s : State) (source : Nat) : State :=
  requestEventFor s (.nodes [source])

def transcriptInvalidation (s : State) : State :=
  { s with transcriptEpoch := s.transcriptEpoch + 1 }

theorem newer_data_rejects_old_read (s : State) (read : Read)
    (h : read.dataEpoch < s.dataEpoch) : publish s read = s := by
  have hne : s.dataEpoch ≠ read.dataEpoch := by omega
  simp [publish, publishFor, acceptsFor, acceptsReloadFor, hne]

theorem uncached_request_event_rejects_old_read (s : State) :
    publish (requestEvent s) (beginRead s) = requestEvent s := by
  simp [publish, publishFor, requestEvent, requestEventFor, touchEvents, beginRead,
    acceptsFor, eventsAcceptFor]

theorem request_event_does_not_invalidate_lineage_cache (s : State) :
    (requestEvent s).provenanceEpoch = s.provenanceEpoch := by
  rfl

theorem transcript_invalidation_preserves_read (s : State) :
    accepts (transcriptInvalidation s) (beginRead s) = true := by
  simp [transcriptInvalidation, beginRead, accepts, acceptsFor, acceptsReloadFor,
    eventsAcceptFor]

theorem accepted_publication_advances_data_epoch (s : State) :
    (publish s (beginRead s)).dataEpoch = s.dataEpoch + 1 := by
  simp [publish, publishFor, beginRead, acceptsFor, acceptsReloadFor, eventsAcceptFor,
    dataChange]

theorem same_owner_data_rejects_scoped_read (s : State) (node : Nat) :
    acceptsFor (dataChange s (.nodes [node])) (beginRead s) (.nodes [node]) = false := by
  simp [acceptsFor, acceptsReloadFor, dataChange, touch, beginRead]

theorem same_owner_request_event_rejects_scoped_read (s : State) (node : Nat) :
    acceptsFor (requestEventFor s (.nodes [node])) (beginRead s) (.nodes [node]) = false := by
  simp [acceptsFor, acceptsReloadFor, eventsAcceptFor, requestEventFor, touchEvents, beginRead]

theorem unclassified_request_event_rejects_scoped_read (s : State) (node : Nat) :
    acceptsFor (requestEvent s) (beginRead s) (.nodes [node]) = false := by
  simp [acceptsFor, acceptsReloadFor, eventsAcceptFor, requestEvent, requestEventFor,
    touchEvents, beginRead]

theorem unrelated_data_preserves_scoped_read (s : State) (node other : Nat)
    (h : node ≠ other) :
    acceptsFor (dataChange s (.nodes [other])) (beginRead s) (.nodes [node]) = true := by
  simp [acceptsFor, acceptsReloadFor, eventsAcceptFor, dataChange, touch, beginRead, h]

theorem unrelated_request_event_preserves_scoped_read (s : State) (node other : Nat)
    (h : node ≠ other) :
    acceptsFor (requestEventFor s (.nodes [other])) (beginRead s) (.nodes [node]) = true := by
  simp [acceptsFor, acceptsReloadFor, eventsAcceptFor, requestEventFor, touchEvents,
    beginRead, h]

/-- NoStaleCommit for reloads: a publication into the reload's node since its
capture rejects it, so an older read never replaces newer published rows. -/
theorem same_owner_publication_rejects_reload (s : State) (node : Nat) :
    acceptsReloadFor (dataChange s (.nodes [node])) (beginRead s) (.nodes [node]) = false := by
  simp [acceptsReloadFor, dataChange, touch, beginRead]

theorem full_replacement_rejects_reload (s : State) (scope : Scope) :
    acceptsReloadFor (dataChange s .all) (beginRead s) scope = false := by
  cases scope <;> simp [acceptsReloadFor, dataChange, touch, beginRead]

/-- A request event of any footprint leaves every reload's fence in place. -/
theorem request_event_preserves_reload (s : State) (event scope : Scope) :
    acceptsReloadFor (requestEventFor s event) (beginRead s) scope = true := by
  cases event <;> cases scope <;>
    simp [acceptsReloadFor, requestEventFor, touchEvents, beginRead]

def requestEvents (s : State) : List Scope → State
  | [] => s
  | event :: rest => requestEvents (requestEventFor s event) rest

theorem request_events_preserve_publication_epochs (s : State) (events : List Scope) :
    (requestEvents s events).dataEpoch = s.dataEpoch ∧
      (requestEvents s events).sharedEpoch = s.sharedEpoch ∧
      (requestEvents s events).nodeEpoch = s.nodeEpoch := by
  induction events generalizing s with
  | nil => simp [requestEvents]
  | cons event rest ih =>
    have h := ih (requestEventFor s event)
    cases event <;> simpa [requestEvents, requestEventFor, touchEvents] using h

/-- EventualPublication's safety half: however many request events, of any
footprint, arrive while a reload is in flight, it publishes. -/
theorem arbitrarily_many_request_events_preserve_reload
    (s : State) (events : List Scope) (scope : Scope) :
    acceptsReloadFor (requestEvents s events) (beginRead s) scope = true := by
  obtain ⟨hData, hShared, hNodes⟩ := request_events_preserve_publication_epochs s events
  cases scope <;> simp [acceptsReloadFor, beginRead, hData, hShared, hNodes]

/-- An observer event from one node's subscription does not reject a read of
another node, whether reload or patch. -/
theorem observer_event_preserves_other_node_read (s : State) (node source : Nat)
    (h : node ≠ source) :
    acceptsFor (observerEvent s source) (beginRead s) (.nodes [node]) = true ∧
      acceptsReloadFor (observerEvent s source) (beginRead s) (.nodes [node]) = true := by
  exact ⟨unrelated_request_event_preserves_scoped_read s node source h,
    request_event_preserves_reload s _ _⟩

/-- A patch read before an event of its own node is still rejected. -/
theorem observer_event_rejects_own_node_patch (s : State) (node : Nat) :
    acceptsFor (observerEvent s node) (beginRead s) (.nodes [node]) = false :=
  same_owner_request_event_rejects_scoped_read s node

def unrelatedTraffic (s : State) (other : Nat) : Nat → State
  | 0 => s
  | n + 1 => dataChange (unrelatedTraffic s other n) (.nodes [other])

theorem arbitrarily_many_unrelated_changes_preserve_scoped_read
    (s : State) (node other count : Nat) (h : node ≠ other) :
    acceptsFor (unrelatedTraffic s other count) (beginRead s) (.nodes [node]) = true := by
  have shared : (unrelatedTraffic s other count).sharedEpoch = s.sharedEpoch := by
    induction count with
    | zero => rfl
    | succ n ih => simpa [unrelatedTraffic, dataChange, touch] using ih
  have owned : (unrelatedTraffic s other count).nodeEpoch node = s.nodeEpoch node := by
    clear shared
    induction count with
    | zero => rfl
    | succ n ih => simpa [unrelatedTraffic, dataChange, touch, h] using ih
  have events : (unrelatedTraffic s other count).eventSharedEpoch = s.eventSharedEpoch ∧
      (unrelatedTraffic s other count).eventNodeEpoch = s.eventNodeEpoch := by
    clear shared owned
    induction count with
    | zero => exact ⟨rfl, rfl⟩
    | succ n ih => simpa [unrelatedTraffic, dataChange, touch] using ih
  simp [acceptsFor, acceptsReloadFor, eventsAcceptFor, beginRead, shared, owned, events.1,
    events.2]

theorem scoped_publication_preserves_other_owner (s : State) (read : Read) (node other : Nat)
    (h : other ≠ node) :
    (publishFor s read (.nodes [node])).nodeEpoch other = s.nodeEpoch other := by
  simp only [publishFor]
  split <;> simp [dataChange, touch, h]

/-- A configuration row carries its canonical owner independently of where the
desktop observed it. Scoped replacement removes only rows owned by the target;
an unscoped local-replica row for another owner remains valid. -/
structure ScopedConfigRow where
  owner : Nat
  key : String
  value : String
  source : Option Nat
  deriving DecidableEq, Repr

/-- Companion observations do not carry their own canonical owner. Their
`source` is taken from the parent document that supplied them. A scoped reload
replaces only observations with known target provenance and keeps unknown rows
for a future full reconciliation. -/
structure ScopedObservationRow where
  key : String
  value : String
  source : Option Nat
  deriving DecidableEq, Repr

def upsertConfigRow (rows : List ScopedConfigRow) (incoming : ScopedConfigRow) :
    List ScopedConfigRow :=
  match rows with
  | [] => [incoming]
  | row :: rest =>
    if row.owner = incoming.owner ∧ row.key = incoming.key then
      incoming :: rest
    else
      row :: upsertConfigRow rest incoming

def upsertConfigRows (rows : List ScopedConfigRow) : List ScopedConfigRow →
    List ScopedConfigRow
  | [] => rows
  | incoming :: rest => upsertConfigRows (upsertConfigRow rows incoming) rest

def upsertObservationRow (rows : List ScopedObservationRow)
    (incoming : ScopedObservationRow) : List ScopedObservationRow :=
  match rows with
  | [] => [incoming]
  | row :: rest =>
    if row.source = incoming.source ∧ row.key = incoming.key then
      incoming :: rest
    else
      row :: upsertObservationRow rest incoming

def upsertObservationRows (rows : List ScopedObservationRow) :
    List ScopedObservationRow → List ScopedObservationRow
  | [] => rows
  | incoming :: rest => upsertObservationRows (upsertObservationRow rows incoming) rest

def replaceConfigScope (scope : Nat) (current incoming : List ScopedConfigRow) :
    List ScopedConfigRow :=
  upsertConfigRows (current.filter (fun row => row.owner != scope))
    (incoming.filter (fun row => row.owner == scope))

def replaceObservationScope (scope : Nat)
    (current incoming : List ScopedObservationRow) : List ScopedObservationRow :=
  upsertObservationRows (current.filter (fun row => row.source != some scope))
    (incoming.filter (fun row => row.source == some scope))

def scopedObservationFor (scope : Nat) (key : String)
    (rows : List ScopedObservationRow) : Option ScopedObservationRow :=
  rows.find? (fun row => row.key == key && row.source == some scope)

theorem upsert_config_row_preserves_foreign
    (row : ScopedConfigRow) (scope : Nat) (rows : List ScopedConfigRow)
    (incoming : ScopedConfigRow) (hincoming : incoming.owner = scope)
    (hrow : row.owner ≠ scope) :
    row ∈ upsertConfigRow rows incoming ↔ row ∈ rows := by
  induction rows with
  | nil =>
    have hne : row ≠ incoming := by
      intro heq
      apply hrow
      rw [heq]
      exact hincoming
    simp [upsertConfigRow, hne]
  | cons head tail ih =>
    by_cases hmatch : head.owner = incoming.owner ∧ head.key = incoming.key
    · have hneIncoming : row ≠ incoming := by
        intro heq
        apply hrow
        rw [heq]
        exact hincoming
      have hneHead : row ≠ head := by
        intro heq
        apply hrow
        rw [heq, hmatch.1, hincoming]
      simp [upsertConfigRow, hmatch, hneIncoming, hneHead]
    · simp [upsertConfigRow, hmatch, ih]

theorem upsert_config_rows_preserves_foreign
    (row : ScopedConfigRow) (scope : Nat) (rows incoming : List ScopedConfigRow)
    (hincoming : ∀ item ∈ incoming, item.owner = scope)
    (hrow : row.owner ≠ scope) :
    row ∈ upsertConfigRows rows incoming ↔ row ∈ rows := by
  induction incoming generalizing rows with
  | nil => simp [upsertConfigRows]
  | cons item rest ih =>
    have howner := hincoming item (by simp)
    simp only [upsertConfigRows]
    have hrest : ∀ next ∈ rest, next.owner = scope := by
      intro next hnext
      exact hincoming next (by simp [hnext])
    exact (ih (upsertConfigRow rows item) hrest).trans
      (upsert_config_row_preserves_foreign row scope rows item howner hrow)


theorem scoped_config_replacement_preserves_other_owner
    (scope : Nat) (current incoming : List ScopedConfigRow) (row : ScopedConfigRow)
    (h : row.owner ≠ scope) :
    row ∈ replaceConfigScope scope current incoming ↔ row ∈ current := by
  unfold replaceConfigScope
  have hIncoming : ∀ item ∈ incoming.filter (fun item => item.owner == scope),
      item.owner = scope := by
    intro item hitem
    simp at hitem
    exact hitem.2
  rw [upsert_config_rows_preserves_foreign row scope _ _ hIncoming h]
  simp [h]


theorem upsert_observation_row_preserves_foreign
    (row : ScopedObservationRow) (scope : Nat) (rows : List ScopedObservationRow)
    (incoming : ScopedObservationRow) (hincoming : incoming.source = some scope)
    (hrow : row.source ≠ some scope) :
    row ∈ upsertObservationRow rows incoming ↔ row ∈ rows := by
  induction rows with
  | nil =>
    have hne : row ≠ incoming := by
      intro heq
      apply hrow
      rw [heq]
      exact hincoming
    simp [upsertObservationRow, hne]
  | cons head tail ih =>
    by_cases hmatch : head.source = incoming.source ∧ head.key = incoming.key
    · have hneIncoming : row ≠ incoming := by
        intro heq
        apply hrow
        rw [heq]
        exact hincoming
      have hneHead : row ≠ head := by
        intro heq
        apply hrow
        rw [heq, hmatch.1, hincoming]
      simp [upsertObservationRow, hmatch, hneIncoming, hneHead]
    · simp [upsertObservationRow, hmatch, ih]

theorem upsert_observation_rows_preserves_foreign
    (row : ScopedObservationRow) (scope : Nat) (rows incoming : List ScopedObservationRow)
    (hincoming : ∀ item ∈ incoming, item.source = some scope)
    (hrow : row.source ≠ some scope) :
    row ∈ upsertObservationRows rows incoming ↔ row ∈ rows := by
  induction incoming generalizing rows with
  | nil => simp [upsertObservationRows]
  | cons item rest ih =>
    have howner := hincoming item (by simp)
    simp only [upsertObservationRows]
    have hrest : ∀ next ∈ rest, next.source = some scope := by
      intro next hnext
      exact hincoming next (by simp [hnext])
    exact (ih (upsertObservationRow rows item) hrest).trans
      (upsert_observation_row_preserves_foreign row scope rows item howner hrow)


theorem scoped_observation_replacement_preserves_other_source
    (scope : Nat) (current incoming : List ScopedObservationRow)
    (row : ScopedObservationRow) (h : row.source ≠ some scope) :
    row ∈ replaceObservationScope scope current incoming ↔ row ∈ current := by
  unfold replaceObservationScope
  have hIncoming : ∀ item ∈ incoming.filter (fun item => item.source == some scope),
      item.source = some scope := by
    intro item hitem
    simp at hitem
    exact hitem.2
  rw [upsert_observation_rows_preserves_foreign row scope _ _ hIncoming h]
  simp [h]


theorem unknown_observation_is_not_node_scoped
    (scope : Nat) (key : String) (rows : List ScopedObservationRow)
    (h : ∀ row ∈ rows, row.source ≠ some scope) :
    scopedObservationFor scope key rows = none := by
  unfold scopedObservationFor
  apply List.find?_eq_none.mpr
  intro row hrow
  simp [h row hrow]

structure Recovery where
  full : Bool
  pendingScopes : List Nat
  generation : Nat
  retryScheduled : Bool
  deriving DecidableEq, Repr

def requestScope (r : Recovery) (scope : Nat) : Recovery :=
  if r.full then { r with generation := r.generation + 1 }
  else if scope ∈ r.pendingScopes then { r with generation := r.generation + 1 }
  else { r with pendingScopes := r.pendingScopes ++ [scope], generation := r.generation + 1 }

def requestFull (r : Recovery) : Recovery :=
  { r with full := true, pendingScopes := [], generation := r.generation + 1 }

def takeBatch (r : Recovery) : Recovery × List Nat :=
  ({ r with pendingScopes := [] }, r.pendingScopes)

/-- A failed batch retains the failed and not-yet-processed scopes. It schedules
the existing observer timer without publishing another wake notification. -/
def failBatch (r : Recovery) (remainingScopes : List Nat) : Recovery :=
  if r.full then { r with retryScheduled := true }
  else { r with pendingScopes := r.pendingScopes ++ remainingScopes, retryScheduled := true }

def retryTimer (r : Recovery) : Recovery := { r with retryScheduled := false }

theorem failed_batch_retains_every_scope (r : Recovery) (scopes : List Nat)
    (scope : Nat) (h : scope ∈ scopes) :
    scope ∈ (failBatch r scopes).pendingScopes ∨ (failBatch r scopes).full = true := by
  by_cases hf : r.full
  · simp [failBatch, hf]
  · simp [failBatch, hf, List.mem_append, h]

theorem failed_batch_does_not_self_notify (r : Recovery) (scopes : List Nat) :
    (failBatch r scopes).generation = r.generation := by
  by_cases hf : r.full <;> simp [failBatch, hf]

theorem request_during_retry_is_retained (r : Recovery) (scopes : List Nat) (scope : Nat) :
    (scope ∈ (requestScope (failBatch r scopes) scope).pendingScopes ∨
      (requestScope (failBatch r scopes) scope).full = true) ∧
      (requestScope (failBatch r scopes) scope).retryScheduled := by
  by_cases hf : r.full
  · simp [failBatch, requestScope, hf]
  · by_cases hmem : scope ∈ r.pendingScopes ++ scopes
    · simp [failBatch, requestScope, hf, hmem]
    · simp [failBatch, requestScope, hf, hmem]

theorem timer_retry_keeps_pending_work (r : Recovery) :
    (retryTimer r).pendingScopes = r.pendingScopes ∧
      (retryTimer r).full = r.full ∧ !(retryTimer r).retryScheduled := by
  simp [retryTimer]

theorem request_event_does_not_change_lineage (s : State) :
    (requestEvent s).provenanceEpoch = s.provenanceEpoch := by rfl

/-- A delivered tool-call change. The observer projection drops tool-call rows,
yet request lineage is derived from them, and the envelope names only the
tool call's document, not the requests whose lineage it moves. The change is
therefore an unclassified request event on the patch fence and moves the
lineage-cache revision. It publishes no rows, so reloads keep their fence. -/
def provenanceInvalidation (s : State) : State :=
  { requestEventFor s .all with provenanceEpoch := s.provenanceEpoch + 1 }

theorem provenance_invalidation_rejects_patch (s : State) (scope : Scope) :
    acceptsFor (provenanceInvalidation s) (beginRead s) scope = false := by
  cases scope <;>
    simp [provenanceInvalidation, acceptsFor, acceptsReloadFor, eventsAcceptFor,
      requestEventFor, touchEvents, beginRead]

theorem provenance_invalidation_preserves_reload (s : State) (scope : Scope) :
    acceptsReloadFor (provenanceInvalidation s) (beginRead s) scope = true := by
  have h := request_event_preserves_reload s .all scope
  cases scope <;> simpa [provenanceInvalidation, acceptsReloadFor, requestEventFor,
    touchEvents] using h

theorem provenance_invalidation_moves_lineage (s : State) :
    (provenanceInvalidation s).provenanceEpoch = s.provenanceEpoch + 1 := rfl

/-! The fence footprint of a node reload. A row is keyed by its merge key and
carries its canonical owner, if known. -/

structure FootprintRow where
  key : String
  owner : Option Nat
  deriving DecidableEq, Repr

/-- A reload of `node` publishes only the snapshot rows the node owns; rows the
runtime returns for other owners or without an owner are dropped before the
footprint is computed, so they cannot fence any other node's reads. -/
def publishedRows (node : Nat) (incoming : List FootprintRow) : List FootprintRow :=
  incoming.filter (fun row => row.owner == some node)

/-- Current rows a published row replaces by merge key. -/
def overwrittenRows (node : Nat) (current incoming : List FootprintRow) :
    List FootprintRow :=
  current.filter (fun row => (publishedRows node incoming).any (fun p => p.key == row.key))

/-- The epochs the reload moves: its node and the owner of every row it
overwrites. Overwriting a row with no known owner may change facts a read of
any scope holds, so it moves every scope. -/
def reloadFootprint (node : Nat) (current incoming : List FootprintRow) : Scope :=
  let overwritten := overwrittenRows node current incoming
  if overwritten.any (fun row => row.owner.isNone) then .all
  else .nodes (node :: overwritten.filterMap FootprintRow.owner)

/-- The epochs the reload checks: the shared epoch, its node and every known
owner of a row it overwrites. A publication that changed a row without a known
owner had that row in its own footprint and so moved the shared epoch, which
this check sees. Checking every scope instead lets steady publications of
unrelated nodes reject a widened reload, which never publishes and so never
takes ownership of the row (`ObserverResync` TLA, `WidenedCheckAll`). -/
def reloadCheck (node : Nat) (current incoming : List FootprintRow) : Scope :=
  .nodes (node :: (overwrittenRows node current incoming).filterMap FootprintRow.owner)

/-- The store after the reload: the node's previous rows and every overwritten
row are replaced by the published rows. -/
def reloadRows (node : Nat) (current incoming : List FootprintRow) : List FootprintRow :=
  current.filter (fun row => row.owner != some node &&
      !(publishedRows node incoming).any (fun p => p.key == row.key)) ++
    publishedRows node incoming

theorem foreign_rows_are_not_published (node : Nat) (incoming foreign : List FootprintRow)
    (h : ∀ row ∈ foreign, row.owner ≠ some node) :
    publishedRows node (incoming ++ foreign) = publishedRows node incoming := by
  have hnil : foreign.filter (fun row => row.owner == some node) = [] := by
    apply List.filter_eq_nil_iff.mpr
    intro row hrow
    simp [h row hrow]
  simp [publishedRows, List.filter_append, hnil]

/-- Rows the runtime returns for other owners change neither what the reload
moves nor what it checks. -/
theorem foreign_rows_have_no_footprint (node : Nat) (current incoming foreign : List FootprintRow)
    (h : ∀ row ∈ foreign, row.owner ≠ some node) :
    reloadFootprint node current (incoming ++ foreign) = reloadFootprint node current incoming ∧
      reloadCheck node current (incoming ++ foreign) = reloadCheck node current incoming := by
  have hp := foreign_rows_are_not_published node incoming foreign h
  simp [reloadFootprint, reloadCheck, overwrittenRows, hp]

theorem published_rows_owned (node : Nat) (incoming : List FootprintRow) :
    ∀ row ∈ publishedRows node incoming, row.owner = some node := by
  intro row hrow
  simp [publishedRows] at hrow
  exact hrow.2

/-- A published reload takes ownership of every row it overwrote: reloading the
same rows again moves only known owners. An unowned row therefore widens at
most the one reload that first publishes over it. -/
theorem reload_converts_unowned_rows (node : Nat) (current incoming : List FootprintRow) :
    reloadFootprint node (reloadRows node current incoming) incoming ≠ .all := by
  have hkept : (current.filter (fun row => row.owner != some node &&
      !(publishedRows node incoming).any (fun p => p.key == row.key))).filter
        (fun row => (publishedRows node incoming).any (fun p => p.key == row.key)) = [] := by
    apply List.filter_eq_nil_iff.mpr
    intro row hrow
    simp only [List.mem_filter, Bool.and_eq_true, Bool.not_eq_true'] at hrow
    simp [hrow.2.2]
  have howned : ∀ row ∈ (publishedRows node incoming).filter
      (fun row => (publishedRows node incoming).any (fun p => p.key == row.key)),
      row.owner = some node := by
    intro row hrow
    exact published_rows_owned node incoming row (List.mem_filter.mp hrow).1
  have hany : ((publishedRows node incoming).filter
      (fun row => (publishedRows node incoming).any (fun p => p.key == row.key))).any
        (fun row => row.owner.isNone) = false := by
    apply List.any_eq_false.mpr
    intro row hrow
    simp [howned row hrow]
  unfold reloadFootprint overwrittenRows reloadRows
  rw [List.filter_append, hkept, List.nil_append]
  simp only [hany]
  simp

/-- Publications into unrelated nodes never reject a reload, even one that
overwrites an unowned row and so publishes into every scope. -/
theorem unrelated_publication_preserves_widened_reload (s : State) (node other : Nat)
    (current incoming : List FootprintRow)
    (h : other ∉ node :: (overwrittenRows node current incoming).filterMap FootprintRow.owner) :
    acceptsReloadFor (dataChange s (.nodes [other])) (beginRead s)
      (reloadCheck node current incoming) = true := by
  simp only [reloadCheck, acceptsReloadFor, dataChange, touch, beginRead]
  simp only [decide_true, Bool.true_and, List.all_eq_true, beq_iff_eq]
  intro member hmember
  have hne : member ≠ other := by
    intro heq
    exact h (heq ▸ hmember)
  simp [hne, List.mem_singleton]

/-- An unclassified publication since the capture rejects every reload. -/
theorem unclassified_publication_rejects_widened_reload (s : State) (node : Nat)
    (current incoming : List FootprintRow) :
    acceptsReloadFor (dataChange s .all) (beginRead s) (reloadCheck node current incoming) =
      false :=
  full_replacement_rejects_reload s _

end ClientDataPublication

namespace ClientLiveDelta

/-- A live cursor names the exact authorized session, physical request,
execution generation and provider source. Native encoding must bind all of
these coordinates. Store revisions are observation notices, not this identity:
an unrelated CRDT arrival cannot invalidate the live source.

This permits updating only the live overlay. A bounded periodic session read
still reconciles history, tools and metadata; a source change or terminal
request requires that read immediately. No delta claims history coverage. -/
def accepts (base current : Option Nat) (terminal : Bool) : Bool :=
  !terminal && base.isSome && decide (base = current)

theorem same_live_source_accepts (cursor : Nat) :
    accepts (some cursor) (some cursor) false = true := by simp [accepts]

theorem terminal_requires_snapshot (base current : Option Nat) :
    accepts base current true = false := by simp [accepts]

theorem missing_base_requires_snapshot (current : Option Nat) (terminal : Bool) :
    accepts none current terminal = false := by simp [accepts]

theorem changed_source_requires_snapshot (base current : Option Nat)
    (terminal : Bool) (h : base ≠ current) :
    accepts base current terminal = false := by simp [accepts, h]

end ClientLiveDelta

namespace ClientStoreNotice

/-- Producers release the store lock before notifying watchers, so delivery can
be reordered. A watcher accepts only a newer committed store version. -/
structure Notice where
  version : Nat
  provenance : Nat
  requestEvent : Nat
  deriving DecidableEq, Repr

def deliver (current incoming : Notice) : Notice :=
  if current.version < incoming.version then incoming else current

theorem delivery_never_regresses (current incoming : Notice) :
    current.version ≤ (deliver current incoming).version := by
  unfold deliver
  split <;> omega

theorem stale_delivery_is_ignored (current incoming : Notice)
    (h : incoming.version ≤ current.version) : deliver current incoming = current := by
  simp [deliver, Nat.not_lt.mpr h]

theorem newer_delivery_keeps_entire_revision (current incoming : Notice)
    (h : current.version < incoming.version) : deliver current incoming = incoming := by
  simp [deliver, h]

end ClientStoreNotice
namespace EnrollmentActivationObservation

structure Authority where
  request : Nat
  digest : Nat
  generation : Nat
  expiresAt : Nat
  deriving DecidableEq, Repr

inductive Failure where
  | authentication | route
  deriving DecidableEq, Repr

structure State where
  current : Option Authority
  failure : Option Failure
  deriving DecidableEq, Repr

/-- The observation belongs to one verified request and authorization
generation. Replacing that authority clears the previous diagnostic. -/
def select (s : State) (authority : Option Authority) : State :=
  if s.current = authority then s else ⟨authority, none⟩

def finish (s : State) (authority : Authority) (failure : Option Failure) : State :=
  if s.current = some authority then { s with failure } else s

def project (s : State) (authority : Authority) (approved routeObserved : Bool) : Option Failure :=
  if approved && s.current == some authority then
    s.failure.orElse (fun _ => if routeObserved then some .route else none)
  else none

theorem stale_completion_ignored (s : State) (authority : Authority)
    (failure : Option Failure) (h : s.current ≠ some authority) :
    finish s authority failure = s := by
  simp [finish, h]

theorem success_clears_failure (s : State) (authority : Authority)
    (h : s.current = some authority) :
    (finish s authority none).failure = none := by
  simp [finish, h]

theorem unapproved_has_no_activation_error (s : State) (authority : Authority) :
    project s authority false false = none := by
  simp [project]

end EnrollmentActivationObservation

namespace RuntimeConnectionObservation

inductive Status where
  | connecting | connected | disconnected | denied
  deriving DecidableEq, Repr

inductive Event where
  | sourceStarted | subscriptionsReady | snapshotSucceeded | transportFailed | permissionDenied
  deriving DecidableEq, Repr

/-- A direct runtime connection is established only after subscriptions are armed
and an authenticated snapshot succeeds. A successful empty result is still a
successful snapshot: ACP may filter every row without returning an error. P2P
peer or replication observations do not determine this state. -/
def observe (status : Status) (event : Event) : Status :=
  match event with
  | .sourceStarted | .subscriptionsReady => .connecting
  | .snapshotSucceeded => .connected
  | .transportFailed => .disconnected
  | .permissionDenied => .denied

def blocksAdmission (status : Status) : Bool := decide (status ≠ .connected)

theorem pairing_does_not_decide_runtime_admission (status : Status) (p2p sync : Bool) :
    (if p2p || sync then blocksAdmission status else blocksAdmission status) = blocksAdmission status := by
  cases p2p <;> cases sync <;> rfl

theorem empty_snapshot_is_connected (status : Status) :
    observe status .snapshotSucceeded = .connected := by rfl

theorem subscription_alone_is_not_a_completed_read (status : Status) :
    observe status .subscriptionsReady = .connecting := by rfl

theorem denial_is_not_an_empty_result (status : Status) :
    observe status .permissionDenied = .denied := by rfl

/-- A denial is not terminal. A grant restored on the runtime emits nothing a
denied reader can subscribe to, and a 401 from an expired or rotated bearer is
also reported as a denial, so the observer restarts a denied source at the same
endpoint after `reprobeDelayMs` of its consecutive-denial count. The count grows
on each denial, resets once an authenticated snapshot succeeds, and the delay
doubles from `reprobeBaseMs` up to `reprobeCapMs`, bounding both the probe rate
against a still-denied runtime and the time a restored grant stays unobserved.
Refresh reads outside the source do not move this state: rows they publish do
not establish a connection, and a denial they observe is confirmed by the
source. -/
def reprobeBaseMs : Nat := 1000

def reprobeCapMs : Nat := 30000

def reprobeDelayMs (denials : Nat) : Nat := min (reprobeBaseMs * 2 ^ denials) reprobeCapMs

def nextDenials (denials : Nat) : Event → Nat
  | .permissionDenied => denials + 1
  | .snapshotSucceeded => 0
  | .sourceStarted | .subscriptionsReady | .transportFailed => denials

theorem denied_source_restarts :
    observe .denied .sourceStarted = .connecting := by rfl

theorem reprobe_delay_bounded (denials : Nat) : reprobeDelayMs denials ≤ reprobeCapMs :=
  Nat.min_le_right _ _

theorem reprobe_delay_positive (denials : Nat) : 0 < reprobeDelayMs denials := by
  unfold reprobeDelayMs reprobeBaseMs reprobeCapMs
  exact Nat.lt_min.mpr ⟨Nat.mul_pos (by decide) (Nat.pow_pos (by decide)), by decide⟩

theorem reprobe_delay_monotone (denials : Nat) :
    reprobeDelayMs denials ≤ reprobeDelayMs (denials + 1) := by
  have h : reprobeBaseMs * 2 ^ denials ≤ reprobeBaseMs * 2 ^ (denials + 1) :=
    Nat.mul_le_mul_left _ (Nat.pow_le_pow_right (by decide) (Nat.le_succ _))
  unfold reprobeDelayMs
  simp only [Nat.min_def]
  split <;> split <;> omega

theorem connected_snapshot_resets_reprobe (denials : Nat) :
    nextDenials denials .snapshotSucceeded = 0 := by rfl

end RuntimeConnectionObservation

/-! The observer's per-node load loop and what it shows the user. One loop owns
every node: a queued node starts one load, which captures the store fence,
reads the runtime and publishes through `ClientDataPublication.reloadFor`.
Liveness of the loop (EventualPublication, Convergence) is checked by the
`ObserverResync` TLA model; the theorems here are the per-step safety and
ownership facts it relies on. -/
namespace ObserverResync

open ClientDataPublication
open RuntimeConnectionObservation (Status)

/-- How a finished snapshot load settles. -/
inductive Settlement where
  | published
  /-- A publication into the epochs the reload checks since the capture
  rejected it. The store already shows newer rows; the next load needs no
  back-off. -/
  | superseded
  | transportFailed
  | denied
  deriving DecidableEq, Repr

inductive LoadResult where
  | rows | transportFailed | denied
  deriving DecidableEq, Repr

/-- `check` is the reload's `reloadCheck`. -/
def settlement (s : State) (read : Read) (check : Scope) : LoadResult → Settlement
  | .rows => if acceptsReloadFor s read check then .published else .superseded
  | .transportFailed => .transportFailed
  | .denied => .denied

structure Slot where
  pending : Bool
  /-- The retry timer must fire before the next load (transport back-off). -/
  waiting : Bool
  loading : Bool
  /-- The node's current source has armed every root. While it has not, the
  source's transport task owns progress: it retries the subscription after
  its back-off until every root arms or the runtime denies the User
  (`observe/remote.rs`; this premise is outside the model). -/
  armed : Bool
  status : Status
  deriving DecidableEq, Repr

def initial : Slot :=
  { pending := false, waiting := false, loading := false, armed := false, status := .connecting }

/-- `SourceStarted`: a new source for the node, configured, replaced or
re-probed after a denial. The previous source's load is aborted. -/
def sourceStarted (n : Slot) : Slot :=
  { n with loading := false, armed := false, status := .connecting }

/-- `SubscriptionsReady`: every root is armed, so the snapshot may start. -/
def armed (n : Slot) : Slot := { n with pending := true, armed := true, status := .connecting }

/-- A delivered event, a direct refresh the fence rejected, or an explicit
resync request queues the node; queued requests coalesce into one load. -/
def queue (n : Slot) : Slot := { n with pending := true }

/-- A load starts only on an armed source. A denied slot is never armed. -/
def startable (n : Slot) : Bool := n.pending && !n.waiting && !n.loading && n.armed

def start (n : Slot) : Slot :=
  if startable n then { n with pending := false, loading := true } else n

def timer (n : Slot) : Slot := { n with waiting := false }

/-- A dropped stream aborts the node's load and disarms the source. A queued
load stays queued; it starts once the transport task re-arms the source. -/
def streamFailed (n : Slot) : Slot :=
  { n with loading := false, armed := false, status := .disconnected }

def settle (n : Slot) : Settlement → Slot
  | .published => { n with loading := false, status := .connected }
  | .superseded => { n with loading := false, pending := true }
  | .transportFailed =>
      { n with loading := false, pending := true, waiting := true, status := .disconnected }
  | .denied =>
      { n with loading := false, pending := false, waiting := false, armed := false,
               status := .denied }

/-- A denial the stream reports settles the slot as a denied load does. -/
def streamDenied (n : Slot) : Slot := settle n .denied

/-- What the user sees for a node, and why it is not stuck. -/
inductive Presentation where
  /-- Rows are being loaded, a load is queued, or the source is arming. -/
  | catchingUp
  | current
  /-- Cause: the runtime is unreachable. Next: the retry timer or the
  transport's re-arm, then a load. -/
  | retrying
  /-- Cause: the runtime refused the User. Next: the re-probe schedule. -/
  | denied
  /-- Nothing owns progress. No `Owned` slot presents this. -/
  | stalled
  deriving DecidableEq, Repr

def present (n : Slot) : Presentation :=
  match n.status with
  | .denied => .denied
  | .disconnected =>
      if n.pending || n.waiting || n.loading || !n.armed then .retrying else .stalled
  | .connecting => if n.pending || n.loading || !n.armed then .catchingUp else .stalled
  | .connected => if n.pending || n.loading then .catchingUp else .current

/-- Every connecting or disconnected slot has a queued or running load or an
unarmed source whose transport task owns the re-arm, and a waiting slot is
queued: the user-visible state is progressing or names its cause. -/
def Owned (n : Slot) : Prop :=
  (n.waiting = true → n.pending = true) ∧
    (n.status = .connecting → n.pending = true ∨ n.loading = true ∨ n.armed = false) ∧
    (n.status = .disconnected → n.pending = true ∨ n.loading = true ∨ n.armed = false)

theorem owned_never_stalls (n : Slot) (h : Owned n) : present n ≠ .stalled := by
  obtain ⟨_, hConnecting, hDisconnected⟩ := h
  cases n with
  | mk pending waiting loading armed status =>
    cases status <;> cases pending <;> cases waiting <;> cases loading <;> cases armed <;>
      simp_all [present]

/-- An owned connecting or disconnected slot names its next step: a load can
start now, the retry timer is due, a load is running, or the source is
arming. -/
theorem owned_names_next_step (n : Slot) (h : Owned n)
    (hs : n.status = .connecting ∨ n.status = .disconnected) :
    startable n = true ∨ n.waiting = true ∨ n.loading = true ∨ n.armed = false := by
  obtain ⟨hWaiting, hConnecting, hDisconnected⟩ := h
  cases n with
  | mk pending waiting loading armed status =>
    rcases hs with hs | hs <;> subst hs <;>
      cases pending <;> cases waiting <;> cases loading <;> cases armed <;>
      simp_all [startable]

theorem initial_owned : Owned initial := by
  simp [Owned, initial]

theorem source_started_owned (n : Slot) (h : Owned n) : Owned (sourceStarted n) := by
  cases n; simp_all [Owned, sourceStarted]

theorem armed_owned (n : Slot) : Owned (armed n) := by
  simp [Owned, armed]

theorem queue_owned (n : Slot) (h : Owned n) : Owned (queue n) := by
  cases n; simp_all [Owned, queue]

theorem start_owned (n : Slot) (h : Owned n) : Owned (start n) := by
  unfold start
  split
  · cases n; simp_all [Owned, startable]
  · exact h

theorem settle_owned (n : Slot) (h : Owned n) (outcome : Settlement) :
    Owned (settle n outcome) := by
  cases n; cases outcome <;> simp_all [Owned, settle]

theorem stream_failed_owned (n : Slot) (h : Owned n) : Owned (streamFailed n) := by
  cases n; simp_all [Owned, streamFailed]

theorem stream_denied_owned (n : Slot) (h : Owned n) : Owned (streamDenied n) :=
  settle_owned n h .denied

theorem timer_owned (n : Slot) (h : Owned n) : Owned (timer n) := by
  cases n; simp_all [Owned, timer]

/-- A dropped stream keeps a queued load from starting until the source
re-arms; the re-arm queues it and it starts unless the retry timer is due. -/
theorem stream_failure_waits_for_arm (n : Slot) :
    startable (streamFailed n) = false ∧
      startable (armed (streamFailed n)) = !n.waiting := by
  simp [startable, streamFailed, armed]

theorem denied_never_starts (n : Slot) : startable (streamDenied n) = false := by
  simp [startable, streamDenied, settle]

/-- A superseded load queues the node again with no back-off. -/
theorem superseded_requeues_without_delay (n : Slot) :
    (settle n .superseded).pending = true ∧ (settle n .superseded).waiting = n.waiting := by
  simp [settle]

/-- Only a transport failure arms the retry timer. -/
theorem only_transport_failure_waits (n : Slot) (outcome : Settlement)
    (h : outcome ≠ .transportFailed) (hw : n.waiting = false) :
    (settle n outcome).waiting = false := by
  cases outcome <;> simp_all [settle]

/-- A safety step of EventualPublication, not the liveness property itself
(that is the TLA model's): a load whose read began before any number of
request events, of any footprint, publishes and shows the node connected. -/
theorem request_events_never_supersede (s : State) (events : List Scope) (check : Scope)
    (n : Slot) :
    settlement (requestEvents s events) (beginRead s) check .rows = .published ∧
      (settle n (settlement (requestEvents s events) (beginRead s) check .rows)).status =
        .connected := by
  have h := arbitrarily_many_request_events_preserve_reload s events check
  simp [settlement, h, settle]

/-- NoStaleCommit through the loop: a publication into the node since capture
supersedes the load, and the node is queued for a fresh read. -/
theorem publication_supersedes_and_requeues (s : State) (node : Nat) (n : Slot) :
    settlement (dataChange s (.nodes [node])) (beginRead s) (.nodes [node]) .rows =
        .superseded ∧
      (settle n (settlement (dataChange s (.nodes [node])) (beginRead s) (.nodes [node])
        .rows)).pending = true := by
  have h := same_owner_publication_rejects_reload s node
  simp [settlement, h, settle]

/-- Queued requests coalesce: however many arrive while a load runs, the node
is queued once and the next `start` begins one load. This bounds hosted reads
by the batches of reasons to read, not below them: a node whose runtime emits
a change during every load (a lease renewal) is read back to back. -/
def queueAll (n : Slot) : Nat → Slot
  | 0 => n
  | k + 1 => queue (queueAll n k)

theorem queued_requests_coalesce (n : Slot) (k : Nat) :
    queueAll n (k + 1) = queue n := by
  induction k with
  | zero => rfl
  | succ k ih => simp [queueAll] at ih ⊢; rw [ih]; simp [queue]

theorem one_load_per_start (n : Slot) :
    start (start n) = start n := by
  unfold start startable
  by_cases h : n.pending && !n.waiting && !n.loading && n.armed
  · simp [h]
  · simp [h]

end ObserverResync
