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

/-- First-run enters setup only after the local init-config owner explicitly
reports absence, or after a configured home has a complete operator snapshot.
Unreadable init state and partial operator snapshots remain unresolved. Once
setup owns the window, observations cannot restart or dismiss it. -/
def observeFirstRun (phase : FirstRunPhase) (snapshotReady initConfigAbsent savedRemote needsSetup authoritative : Bool) : FirstRunPhase :=
  match phase with
  | .active => .active
  | .done => .done
  | .unknown =>
      if !snapshotReady then .unknown
      else if initConfigAbsent then if savedRemote then .done else .active
      else if !authoritative then .unknown
      else if needsSetup then .active else .done

/-- A saved remote setup survives temporary route unavailability. Readiness
and authorization continue to gate the operations that use that route. -/
theorem saved_remote_bypasses_local_setup (needsSetup authoritative : Bool) :
    observeFirstRun .unknown true true true needsSetup authoritative = .done := by
  simp [observeFirstRun]

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

structure Read where
  dataEpoch : Nat
  requestEventEpoch : Nat
  sharedEpoch : Nat := 0
  nodeEpoch : Nat → Nat := fun _ => 0

def beginRead (s : State) : Read :=
  { dataEpoch := s.dataEpoch, requestEventEpoch := s.requestEventEpoch,
    sharedEpoch := s.sharedEpoch, nodeEpoch := s.nodeEpoch }

/-- The footprint includes every owner whose cached facts may change. Missing
ownership, dropped notifications, and unretained lineage require `all`; an
empty footprint has no node-local effect. -/
def touch (s : State) (scope : Scope) : State :=
  match scope with
  | .all => { s with sharedEpoch := s.sharedEpoch + 1 }
  | .nodes owners =>
      { s with nodeEpoch := fun node => s.nodeEpoch node + if node ∈ owners then 1 else 0 }

def acceptsFor (s : State) (read : Read) (scope : Scope) : Bool :=
  match scope with
  | .all => decide (s.dataEpoch = read.dataEpoch ∧ s.requestEventEpoch = read.requestEventEpoch)
  | .nodes owners =>
      decide (s.sharedEpoch = read.sharedEpoch) &&
        owners.all (fun node => s.nodeEpoch node == read.nodeEpoch node)

def accepts (s : State) (read : Read) : Bool :=
  acceptsFor s read .all

def dataChange (s : State) (scope : Scope) : State :=
  { touch s scope with dataEpoch := s.dataEpoch + 1 }

def publishFor (s : State) (read : Read) (scope : Scope) : State :=
  if acceptsFor s read scope then dataChange s scope else s

def publish (s : State) (read : Read) : State :=
  publishFor s read .all

def requestEventFor (s : State) (scope : Scope) : State :=
  { touch s scope with requestEventEpoch := s.requestEventEpoch + 1 }

def requestEvent (s : State) : State :=
  requestEventFor s .all

def transcriptInvalidation (s : State) : State :=
  { s with transcriptEpoch := s.transcriptEpoch + 1 }

theorem newer_data_rejects_old_read (s : State) (read : Read)
    (h : read.dataEpoch < s.dataEpoch) : publish s read = s := by
  have hne : s.dataEpoch ≠ read.dataEpoch := by omega
  simp [publish, publishFor, acceptsFor, hne]

theorem uncached_request_event_rejects_old_read (s : State) :
    publish (requestEvent s) (beginRead s) = requestEvent s := by
  simp [publish, publishFor, requestEvent, requestEventFor, beginRead, acceptsFor]

theorem request_event_does_not_invalidate_lineage_cache (s : State) :
    (requestEvent s).provenanceEpoch = s.provenanceEpoch := by
  rfl

theorem transcript_invalidation_preserves_read (s : State) :
    accepts (transcriptInvalidation s) (beginRead s) = true := by
  simp [transcriptInvalidation, beginRead, accepts, acceptsFor]

theorem accepted_publication_advances_data_epoch (s : State) :
    (publish s (beginRead s)).dataEpoch = s.dataEpoch + 1 := by
  simp [publish, publishFor, beginRead, acceptsFor, dataChange]

theorem same_owner_data_rejects_scoped_read (s : State) (node : Nat) :
    acceptsFor (dataChange s (.nodes [node])) (beginRead s) (.nodes [node]) = false := by
  simp [acceptsFor, dataChange, touch, beginRead]

theorem same_owner_request_event_rejects_scoped_read (s : State) (node : Nat) :
    acceptsFor (requestEventFor s (.nodes [node])) (beginRead s) (.nodes [node]) = false := by
  simp [acceptsFor, requestEventFor, touch, beginRead]

theorem unclassified_request_event_rejects_scoped_read (s : State) (node : Nat) :
    acceptsFor (requestEvent s) (beginRead s) (.nodes [node]) = false := by
  simp [acceptsFor, requestEvent, requestEventFor, touch, beginRead]

theorem unrelated_data_preserves_scoped_read (s : State) (node other : Nat)
    (h : node ≠ other) :
    acceptsFor (dataChange s (.nodes [other])) (beginRead s) (.nodes [node]) = true := by
  simp [acceptsFor, dataChange, touch, beginRead, h]

theorem unrelated_request_event_preserves_scoped_read (s : State) (node other : Nat)
    (h : node ≠ other) :
    acceptsFor (requestEventFor s (.nodes [other])) (beginRead s) (.nodes [node]) = true := by
  simp [acceptsFor, requestEventFor, touch, beginRead, h]

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
  simp [acceptsFor, beginRead, shared, owned]

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
