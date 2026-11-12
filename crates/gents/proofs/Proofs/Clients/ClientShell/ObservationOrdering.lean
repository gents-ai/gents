import Proofs.Clients.ClientShell.Types

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

/-! Each node's rows live in that node's own store. A store is published only
through its node's own connection: the node's observer loads and deliveries, a
direct refresh of the node and writes that name the node. No store holds another
node's rows and no publication or event moves another node's store (`Fleet`), so
a view of several nodes is a summary per node, never one merged store. Within a
store, publication tracks what a read may replace independently of UI intent and
lineage provenance. -/
namespace ClientDataPublication

structure State where
  dataEpoch : Nat
  provenanceEpoch : Nat
  requestEventEpoch : Nat
  transcriptEpoch : Nat
  deriving DecidableEq, Repr

/-- Runtime documents are read from the node's authenticated database endpoint
under the client DID. The observer arms its GraphQL subscriptions before
requesting the initial snapshot; reconnect, renewal and gaps require a fresh snapshot.
These epochs reject stale publications from those reads. A local replica is never
an alternative source for runtime configuration, sessions or transcript data.
Native subscription delivery may omit permission-denied empty results; document
policy revocation invalidation remains an upstream limitation, not an access grant. -/
structure Read where
  dataEpoch : Nat
  requestEventEpoch : Nat
  deriving DecidableEq, Repr

def beginRead (s : State) : Read :=
  { dataEpoch := s.dataEpoch, requestEventEpoch := s.requestEventEpoch }

/-- A snapshot reload replaces the store with the runtime's rows as of its read.
A request event publishes no rows, so it cannot make the reload older than what
the store shows; the event's own resync request reads again after it. Only a
publication into the store since the capture can, so reloads check the
publication epoch alone. Without this, request events arriving at least once
per load reject every reload of the node and its rows never catch up
(`ObserverResync` TLA, `SustainedEvents`). -/
def acceptsReload (s : State) (read : Read) : Bool :=
  decide (s.dataEpoch = read.dataEpoch)

/-- A patch merges rows into the store, so a row it read before a deletion or
change it was told about would survive the event. Patches also check the
request-event epoch. -/
def eventsAccept (s : State) (read : Read) : Bool :=
  decide (s.requestEventEpoch = read.requestEventEpoch)

def accepts (s : State) (read : Read) : Bool :=
  acceptsReload s read && eventsAccept s read

def dataChange (s : State) : State :=
  { s with dataEpoch := s.dataEpoch + 1 }

def publish (s : State) (read : Read) : State :=
  if accepts s read then dataChange s else s

def reload (s : State) (read : Read) : State :=
  if acceptsReload s read then dataChange s else s

/-- A delivered change on the node's own subscription, or a reconnect gap of
it. Every change on a runtime, including one to a document another node owns,
arrives as an event of that runtime's store; it never moves another store. -/
def requestEvent (s : State) : State :=
  { s with requestEventEpoch := s.requestEventEpoch + 1 }

def transcriptInvalidation (s : State) : State :=
  { s with transcriptEpoch := s.transcriptEpoch + 1 }

theorem newer_data_rejects_old_read (s : State) (read : Read)
    (h : read.dataEpoch < s.dataEpoch) : publish s read = s := by
  have hne : s.dataEpoch ≠ read.dataEpoch := by omega
  simp [publish, accepts, acceptsReload, hne]

theorem uncached_request_event_rejects_old_read (s : State) :
    publish (requestEvent s) (beginRead s) = requestEvent s := by
  simp [publish, requestEvent, beginRead, accepts, acceptsReload, eventsAccept]

theorem request_event_does_not_invalidate_lineage_cache (s : State) :
    (requestEvent s).provenanceEpoch = s.provenanceEpoch := by
  rfl

theorem transcript_invalidation_preserves_read (s : State) :
    accepts (transcriptInvalidation s) (beginRead s) = true := by
  simp [transcriptInvalidation, beginRead, accepts, acceptsReload, eventsAccept]

theorem accepted_publication_advances_data_epoch (s : State) :
    (publish s (beginRead s)).dataEpoch = s.dataEpoch + 1 := by
  simp [publish, beginRead, accepts, acceptsReload, eventsAccept, dataChange]

/-- NoStaleCommit for reloads: a publication into the store since the capture
rejects it, so an older read never replaces newer published rows. -/
theorem publication_rejects_reload (s : State) :
    acceptsReload (dataChange s) (beginRead s) = false := by
  simp [acceptsReload, dataChange, beginRead]

theorem request_event_rejects_patch (s : State) :
    accepts (requestEvent s) (beginRead s) = false := by
  simp [accepts, acceptsReload, eventsAccept, requestEvent, beginRead]

/-- A request event leaves the reload fence in place. -/
theorem request_event_preserves_reload (s : State) :
    acceptsReload (requestEvent s) (beginRead s) = true := by
  simp [acceptsReload, requestEvent, beginRead]

def requestEvents (s : State) : Nat → State
  | 0 => s
  | n + 1 => requestEvent (requestEvents s n)

theorem request_events_preserve_publication_epoch (s : State) (count : Nat) :
    (requestEvents s count).dataEpoch = s.dataEpoch := by
  induction count with
  | zero => rfl
  | succ n ih => simpa [requestEvents, requestEvent] using ih

/-- EventualPublication's safety half: however many request events arrive
while a reload is in flight, it publishes. -/
theorem arbitrarily_many_request_events_preserve_reload (s : State) (count : Nat) :
    acceptsReload (requestEvents s count) (beginRead s) = true := by
  simp [acceptsReload, beginRead, request_events_preserve_publication_epoch]

/-- A delivered tool-call change. The observer projection drops tool-call rows,
yet request lineage is derived from them, and the envelope names only the
tool call's document, not the requests whose lineage it moves. The change is
therefore a request event on the patch fence and moves the lineage-cache
revision. It publishes no rows, so reloads keep their fence. -/
def provenanceInvalidation (s : State) : State :=
  { requestEvent s with provenanceEpoch := s.provenanceEpoch + 1 }

theorem provenance_invalidation_rejects_patch (s : State) :
    accepts (provenanceInvalidation s) (beginRead s) = false := by
  simp [provenanceInvalidation, accepts, acceptsReload, eventsAccept, requestEvent,
    beginRead]

theorem provenance_invalidation_preserves_reload (s : State) :
    acceptsReload (provenanceInvalidation s) (beginRead s) = true := by
  simp [provenanceInvalidation, acceptsReload, requestEvent, beginRead]

theorem provenance_invalidation_moves_lineage (s : State) :
    (provenanceInvalidation s).provenanceEpoch = s.provenanceEpoch + 1 := rfl

/-- Every node's store. A publication, event or reload acts on one node's store
through that node's connection. -/
abbrev Fleet := Nat → State

def Fleet.on (fleet : Fleet) (node : Nat) (step : State → State) : Fleet :=
  fun other => if other = node then step (fleet node) else fleet other

theorem other_node_store_unchanged (fleet : Fleet) (node other : Nat)
    (step : State → State) (h : other ≠ node) :
    fleet.on node step other = fleet other := by
  simp [Fleet.on, h]

/-- However one node's store moves, a read of another node publishes exactly as
it would have: a busy node never supersedes or delays another node's reload or
patch. -/
theorem other_node_preserves_read (fleet : Fleet) (node other : Nat)
    (step : State → State) (read : Read) (h : other ≠ node) :
    accepts (fleet.on node step other) read = accepts (fleet other) read ∧
      acceptsReload (fleet.on node step other) read = acceptsReload (fleet other) read := by
  simp [other_node_store_unchanged fleet node other step h]

/-- Reads queued for the observer: one entry per node whose store asked its
connection to read again. The set is bounded by the configured nodes, so it
never collapses into a read of every node. -/
structure Recovery where
  pendingScopes : List Nat
  generation : Nat
  retryScheduled : Bool
  deriving DecidableEq, Repr

def requestScope (r : Recovery) (scope : Nat) : Recovery :=
  if scope ∈ r.pendingScopes then { r with generation := r.generation + 1 }
  else { r with pendingScopes := r.pendingScopes ++ [scope], generation := r.generation + 1 }

def takeBatch (r : Recovery) : Recovery × List Nat :=
  ({ r with pendingScopes := [] }, r.pendingScopes)

/-- A failed batch retains the failed and not-yet-processed scopes. It schedules
the existing observer timer without publishing another wake notification. -/
def failBatch (r : Recovery) (remainingScopes : List Nat) : Recovery :=
  { r with pendingScopes := r.pendingScopes ++ remainingScopes, retryScheduled := true }

def retryTimer (r : Recovery) : Recovery := { r with retryScheduled := false }

theorem failed_batch_retains_every_scope (r : Recovery) (scopes : List Nat)
    (scope : Nat) (h : scope ∈ scopes) :
    scope ∈ (failBatch r scopes).pendingScopes := by
  simp [failBatch, List.mem_append, h]

theorem failed_batch_does_not_self_notify (r : Recovery) (scopes : List Nat) :
    (failBatch r scopes).generation = r.generation := by
  simp [failBatch]

theorem request_during_retry_is_retained (r : Recovery) (scopes : List Nat) (scope : Nat) :
    scope ∈ (requestScope (failBatch r scopes) scope).pendingScopes ∧
      (requestScope (failBatch r scopes) scope).retryScheduled := by
  by_cases hmem : scope ∈ r.pendingScopes ++ scopes
  · simp [failBatch, requestScope, hmem]
  · simp [failBatch, requestScope, hmem]

theorem timer_retry_keeps_pending_work (r : Recovery) :
    (retryTimer r).pendingScopes = r.pendingScopes ∧ !(retryTimer r).retryScheduled := by
  simp [retryTimer]

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

/-- The desktop's observed connection to one runtime it reaches over
authenticated HTTP as the User. `noEndpoint` is a directory peer with no
operator GraphQL endpoint (a discovery- or enrollment-only peer): nothing can
be observed, and the state says so instead of reading as a connection attempt.
A peer that is absent from the directory has no state.

`refused` is a runtime that said it does not accept this client's credentials:
the access owner classified a request as refused (HTTP 401/403, or a
`PERMISSION_DENIED` envelope). That is the runtime's own answer, not an
inference, so the state names it and the user can act on it (the device is not
enrolled there, its key was rotated, or its grant was removed); the source keeps
reconnecting on `reconnectDelayMs`, so restored access connects by itself.

There is no state inferred from what the client cannot see. Native ACP hides
the documents a reader may not read by design: a reader whose grant was revoked
on the pinned served runtime sees fewer documents, possibly none, and the
runtime does not say why. The client never infers a denial from an omitted or
empty result; a view with nothing visible is an ordinary `connected` view. -/
inductive Status where
  | connecting | connected | disconnected | refused | noEndpoint
  deriving DecidableEq, Repr

/-- `renewed` is the source re-arming its subscriptions after a planned stream
end: the pinned DefraDB HTTP server authenticates a subscription only at its
handshake, so a locally minted bearer's subscription ends at half the bearer
lifetime and re-authenticates. It is not a transport failure. `refused` is the
access owner's classification of a request it rejected (HTTP 401/403 at a
handshake or a read, or a `PERMISSION_DENIED` envelope on a stream): the
source's credentials were not accepted, as with an expired or rotated bearer,
so the source reconnects with freshly minted ones (`reconnectDelayMs`).
`endpointMissing` is the reconcile of a peer that has no operator endpoint. -/
inductive Event where
  | sourceStarted | subscriptionsReady | snapshotSucceeded | transportFailed | refused
  | renewed | endpointMissing
  deriving DecidableEq, Repr

/-- A direct runtime connection is established only after subscriptions are armed
and an authenticated snapshot succeeds. A successful empty result is still a
successful snapshot: ACP may filter every row without returning an error. P2P
peer or replication observations do not determine this state. A planned
renewal keeps the state, whatever it is: the source arms the replacement
subscriptions before it drops the old ones, so no change goes undelivered and a
`connected` snapshot stays current across it (TLA `ConnectedAfterArmedSnapshot`).
A refusal is shown as `refused` until a read is accepted: a reconnect that arms
its subscriptions keeps it, since a runtime that accepts the handshake may still
refuse every read, and showing `connecting` on each re-arm would hide the cause
behind a state that looks like progress. -/
def observe (status : Status) (event : Event) : Status :=
  match event with
  | .sourceStarted => .connecting
  | .subscriptionsReady => if status = .refused then .refused else .connecting
  | .refused => .refused
  | .snapshotSucceeded => .connected
  | .transportFailed => .disconnected
  | .renewed => status
  | .endpointMissing => .noEndpoint

def blocksAdmission (status : Status) : Bool := decide (status ≠ .connected)

theorem empty_snapshot_is_connected (status : Status) :
    observe status .snapshotSucceeded = .connected := by rfl

theorem subscription_alone_is_not_a_completed_read (status : Status) :
    observe status .subscriptionsReady ≠ .connected := by
  cases status <;> simp [observe]

/-- A refusal names itself, never an outage or a connection attempt. -/
theorem refusal_is_named (status : Status) :
    observe status .refused = .refused := by rfl

/-- A re-arm or a renewal of a refused source keeps the refusal shown; only an
accepted read (or another endpoint, or an outage) replaces it. -/
theorem refusal_shown_until_a_read_is_accepted :
    observe .refused .subscriptionsReady = .refused ∧ observe .refused .renewed = .refused :=
  ⟨rfl, rfl⟩

theorem renewal_keeps_connected : observe .connected .renewed = .connected := by rfl

theorem renewal_is_not_an_outage (status : Status) (h : status ≠ .disconnected) :
    observe status .renewed ≠ .disconnected := by
  simpa [observe] using h

/-- Only a completed authenticated snapshot enters `connected`; a renewal can
only keep it. -/
theorem only_snapshot_enters_connected (status : Status) (event : Event)
    (h : status ≠ .connected) (hc : observe status event = .connected) :
    event = .snapshotSucceeded := by
  cases event <;> simp_all [observe]
  split at hc <;> simp_all

/-- Only a transport failure shows an outage. -/
theorem only_transport_failure_disconnects (status : Status) (event : Event)
    (h : status ≠ .disconnected) (hd : observe status event = .disconnected) :
    event = .transportFailed := by
  cases event <;> simp_all [observe]
  split at hd <;> simp_all

theorem missing_endpoint_is_not_connecting (status : Status) :
    observe status .endpointMissing = .noEndpoint := by rfl

/-- How a source's subscription stream ended. `renewalDue` is the planned
re-authentication end of a subscription that keeps its own renewal deadline;
the observer's source takes that cadence (`renewalStartMs`) and never sees it.
`completed` is the runtime sending `complete`, which is not planned by the
client and is reported as a transport failure until a re-arm proves otherwise.
A failure is `refused` exactly when the access owner classifies it so (HTTP
401/403 at the handshake, or a `PERMISSION_DENIED` GraphQL error envelope on the
stream). The pinned served runtime produces neither for a revoked User; see
`SnapshotOutcome`. -/
inductive StreamEnd where
  | renewalDue | completed | failed (refused : Bool)
  deriving DecidableEq, Repr

/-- The event a stream end publishes. A renewal publishes nothing until its
re-arm: `renewed` when the new subscriptions are ready, or the re-arm's own
failure. -/
def streamEndEvent : StreamEnd → Option Event
  | .renewalDue => none
  | .completed => some .transportFailed
  | .failed true => some .refused
  | .failed false => some .transportFailed

theorem renewal_publishes_no_failure : streamEndEvent .renewalDue = none := by rfl

theorem classified_refusal_is_refused : streamEndEvent (.failed true) = some .refused := by
  rfl

/-- The notice a source sends once every subscription of an arm is armed:
`renewed` when the arm replaced a running set, `subscriptionsReady` otherwise.
A source notice carries exactly one event; there is no precedence between
notice fields. -/
def armedEvent (renewal : Bool) : Event := if renewal then .renewed else .subscriptionsReady

/-- Whether the source is armed after a notice of its task, so the observer
may run a snapshot of it. An armed notice arms it; a failure or a refusal
ends the source until its reconnect arms again. The task sends no other event
(the remaining cases leave it as it was): a snapshot result is the observer's
own, and a snapshot that failed for a transport reason leaves the streams armed
and is retried after `snapshotRetryDelayMs`. -/
def armedAfter (wasArmed : Bool) : Event → Bool
  | .subscriptionsReady | .renewed => true
  | .transportFailed | .refused => false
  | .sourceStarted | .snapshotSucceeded | .endpointMissing => wasArmed

theorem armed_notice_schedules_a_snapshot (wasArmed renewal : Bool) :
    armedAfter wasArmed (armedEvent renewal) = true := by
  cases renewal <;> rfl

theorem stream_end_disarms (wasArmed : Bool) (streamEnd : StreamEnd) (event : Event)
    (h : streamEndEvent streamEnd = some event) : armedAfter wasArmed event = false := by
  cases streamEnd with
  | renewalDue => simp [streamEndEvent] at h
  | completed => simp [streamEndEvent] at h; subst h; rfl
  | failed refused => cases refused <;> simp [streamEndEvent] at h <;> subst h <;> rfl

/-- How the source's snapshot read ended. `refused` is an HTTP 401/403 or a
`PERMISSION_DENIED` envelope (the access owner's classification).

External premise: the pinned served runtime answers a User whose grant was
revoked with HTTP 200, no `errors`, and `data` that omits the selected roots;
it also accepts that User's SSE handshake and keeps its streams silent (observe
test `revoked_user_is_answered_with_omitted_roots`). That is native ACP hiding
what the User may not read, not a refusal: the snapshot reader decodes an
omitted root as an empty list, and the read `succeeded`. Classifying it as a
denial would show a state the runtime never asserted; classifying it as a
failure would reconnect a quiet runtime forever. -/
inductive SnapshotOutcome where
  | succeeded | refused | unreachable
  deriving DecidableEq, Repr

def snapshotEvent : SnapshotOutcome → Event
  | .succeeded => .snapshotSucceeded
  | .refused => .refused
  | .unreachable => .transportFailed

/-- Where an event came from: the source task's notice (its stream or its
handshake), or the observer's own snapshot read. -/
inductive Origin where
  | source | snapshot
  deriving DecidableEq, Repr

/-- Whether the event ends the source and schedules its reconnect. A source
whose stream failed for a transport reason, or whose credentials were refused,
has ended; a refused read means the credentials the source armed with are not
accepted, so it reconnects too. A snapshot that failed for a transport reason
does not: the source's streams are still armed and deliver every change, so
only the read is retried, after `snapshotRetryDelayMs` (`armedAfter`,
`ObserverResync.settle .transportFailed`, TLA `FinishLoad`). Reconnecting there
would tear down every healthy subscription for one failed read. -/
def reconnects : Origin → Event → Bool
  | .source, .transportFailed | .source, .refused | .snapshot, .refused => true
  | _, _ => false

theorem snapshot_transport_failure_keeps_the_streams :
    reconnects .snapshot .transportFailed = false := by rfl

theorem every_refusal_reconnects (origin : Origin) : reconnects origin .refused = true := by
  cases origin <;> rfl

/-- Only a failure or a refusal reconnects; an arm, a renewal, a successful
read or a missing endpoint never does. -/
theorem only_failures_reconnect (origin : Origin) (event : Event)
    (h : reconnects origin event = true) : event = .transportFailed ∨ event = .refused := by
  cases origin <;> cases event <;> simp_all [reconnects]

/-- The source task sends a reconnecting event exactly when its notice
disarms the source. -/
theorem source_reconnects_iff_disarmed (event : Event)
    (h : event ≠ .sourceStarted ∧ event ≠ .snapshotSucceeded ∧ event ≠ .endpointMissing) :
    reconnects .source event = !armedAfter true event := by
  cases event <;> simp_all [reconnects, armedAfter]

/-- The flat reconnect delay, and how many consecutive reconnects use it. An
ended source is restarted at the same endpoint after `reconnectDelayMs` of its
consecutive reconnect count. The first `reconnectFlatAttempts` reconnects wait
`reconnectBaseMs`, so a brief outage or a runtime restart is noticed within a
quarter second; later ones double up to `reconnectCapMs`, bounding the dial
rate against a runtime that stays down, keeps refusing or keeps dropping its
streams, and bounding how long a recovered runtime stays unobserved. -/
def reconnectBaseMs : Nat := 250

def reconnectFlatAttempts : Nat := 5

def reconnectCapMs : Nat := 30000

/-- `attempt` counts the consecutive reconnects since the source last stayed
connected for `reconnectStableMs` (`reconnectAttempt`). -/
def reconnectDelayMs (attempt : Nat) : Nat :=
  if attempt < reconnectFlatAttempts then reconnectBaseMs
  else min (reconnectBaseMs * 2 ^ (attempt + 1 - reconnectFlatAttempts)) reconnectCapMs

theorem first_reconnects_are_flat (attempt : Nat) (h : attempt < reconnectFlatAttempts) :
    reconnectDelayMs attempt = reconnectBaseMs := by
  simp [reconnectDelayMs, h]

theorem first_five_reconnects_wait_250ms :
    (List.range 5).map reconnectDelayMs = [250, 250, 250, 250, 250] := by decide

theorem reconnect_delay_doubles_after_the_flat_attempts :
    (List.range 13).map reconnectDelayMs =
      [250, 250, 250, 250, 250, 500, 1000, 2000, 4000, 8000, 16000, 30000, 30000] := by decide

theorem reconnect_delay_bounded (attempt : Nat) : reconnectDelayMs attempt ≤ reconnectCapMs := by
  unfold reconnectDelayMs
  split
  · decide
  · exact Nat.min_le_right _ _

theorem reconnect_delay_positive (attempt : Nat) : 0 < reconnectDelayMs attempt := by
  unfold reconnectDelayMs reconnectBaseMs reconnectCapMs
  split
  · decide
  · exact Nat.lt_min.mpr ⟨Nat.mul_pos (by decide) (Nat.pow_pos (by decide)), by decide⟩

theorem reconnect_delay_monotone (attempt : Nat) :
    reconnectDelayMs attempt ≤ reconnectDelayMs (attempt + 1) := by
  unfold reconnectDelayMs reconnectFlatAttempts reconnectBaseMs reconnectCapMs
  by_cases h : attempt + 1 < 5
  · have h' : attempt < 5 := by omega
    simp [h, h']
  · by_cases h' : attempt < 5
    · have e : attempt = 4 := by omega
      subst e; decide
    · simp only [h, h', if_false]
      have hp : 250 * 2 ^ (attempt + 1 - 5) ≤ 250 * 2 ^ (attempt + 1 + 1 - 5) :=
        Nat.mul_le_mul_left _ (Nat.pow_le_pow_right (by decide) (by omega))
      simp only [Nat.min_def]
      split <;> split <;> omega

/-- The delay never decreases while reconnects continue: once past the flat
attempts it is monotone up to the cap. -/
theorem reconnect_delay_monotone_le (a b : Nat) (h : a ≤ b) :
    reconnectDelayMs a ≤ reconnectDelayMs b := by
  induction b with
  | zero => have : a = 0 := by omega
            subst this; exact Nat.le_refl _
  | succ b ih =>
    rcases Nat.lt_or_ge a (b + 1) with hlt | hge
    · exact Nat.le_trans (ih (by omega)) (reconnect_delay_monotone b)
    · have : a = b + 1 := by omega
      subst this; exact Nat.le_refl _

theorem reconnect_delay_reaches_the_cap (attempt : Nat) (h : 11 ≤ attempt) :
    reconnectDelayMs attempt = reconnectCapMs := by
  apply Nat.le_antisymm (reconnect_delay_bounded attempt)
  have := reconnect_delay_monotone_le 11 attempt h
  exact Nat.le_trans (by decide) this

/-- How long a source must stay connected (armed, with a successful snapshot)
before its consecutive reconnect count resets. A count that reset at a
successful arm, or at a successful snapshot, would let a runtime that accepts
both and then drops the stream (or refuses every read after the arm) be redialed
at the flat rate forever: every cycle would start again at attempt 0. Resetting
only after the connection held for as long as the longest wait bounds the dial
rate of such a runtime at the cap, while a runtime that was up and then
restarts is noticed within `reconnectBaseMs`. It is a duration, not a renewal:
a delegated credential has no renewal (`renewalPeriodMs`). -/
def reconnectStableMs : Nat := 30000

/-- The count a reconnect is scheduled with, given the count so far and how
long the source had been connected when it ended (`none`: it had not connected
since its last reconnect). The count after the reconnect is this plus one. -/
def reconnectAttempt (attempts : Nat) : Option Nat → Nat
  | some connectedMs => if reconnectStableMs ≤ connectedMs then 0 else attempts
  | none => attempts

theorem stable_connection_resets (attempts connectedMs : Nat)
    (h : reconnectStableMs ≤ connectedMs) :
    reconnectDelayMs (reconnectAttempt attempts (some connectedMs)) = reconnectBaseMs := by
  simp [reconnectAttempt, h, reconnectDelayMs, reconnectFlatAttempts]

theorem short_connection_keeps_counting (attempts connectedMs : Nat)
    (h : connectedMs < reconnectStableMs) :
    reconnectAttempt attempts (some connectedMs) = attempts := by
  simp [reconnectAttempt, Nat.not_le.mpr h]

theorem unconnected_reconnect_keeps_counting (attempts : Nat) :
    reconnectAttempt attempts none = attempts := rfl

/-- The counts of `cycles` consecutive reconnects of a runtime that each time
connects for `connectedMs` (or never, with `none`) and then ends again. -/
def flapCount (connected : Option Nat) : Nat → Nat
  | 0 => 0
  | n + 1 => reconnectAttempt (flapCount connected n) connected + 1

theorem flap_count_grows (connected : Option Nat)
    (h : ∀ ms, connected = some ms → ms < reconnectStableMs) (n : Nat) :
    flapCount connected n = n := by
  induction n with
  | zero => rfl
  | succ n ih =>
    cases connected with
    | none => simp [flapCount, ih, reconnectAttempt]
    | some ms => simp [flapCount, ih, short_connection_keeps_counting _ _ (h ms rfl)]

/-- A runtime that never holds a connection for `reconnectStableMs` (it drops
every stream right after the arm and snapshot, refuses every read, or refuses
every handshake) is dialed at the flat rate at most `reconnectFlatAttempts`
times in a row, and from the twelfth reconnect on only once per cap. -/
theorem flapping_runtime_backs_off (connected : Option Nat)
    (h : ∀ ms, connected = some ms → ms < reconnectStableMs) (n : Nat) :
    (reconnectFlatAttempts ≤ n → reconnectBaseMs < reconnectDelayMs (flapCount connected n)) ∧
      (11 ≤ n → reconnectDelayMs (flapCount connected n) = reconnectCapMs) := by
  rw [flap_count_grows connected h n]
  refine ⟨fun hn => ?_, reconnect_delay_reaches_the_cap n⟩
  have := reconnect_delay_monotone_le 5 n hn
  exact Nat.lt_of_lt_of_le (by decide) this

/-- A snapshot whose read failed for a transport reason, or whose result
raced a newer store write, is retried after this delay while its source stays
armed. It is not a reconnect: the streams are still armed. -/
def snapshotRetryDelayMs : Nat := 250

theorem snapshot_retry_positive : 0 < snapshotRetryDelayMs := by decide

/-- Half the locally minted bearer lifetime (60s): how long the source keeps
an armed subscription set before it re-authenticates by arming a replacement. -/
def renewalPeriodMs : Nat := 30000

/-- When the next renewal starts, given when the current one started and how
long its arm took. The period is measured from the completed arm, not from a
handshake's start: measured from the handshake, an arm slower than the period
would leave the next renewal already due, and the source would re-arm and
resnapshot without pause while reading `connected`. -/
def renewalStartMs (startedMs armMs : Nat) : Nat := startedMs + armMs + renewalPeriodMs

theorem renewals_are_spaced_by_a_period (startedMs armMs : Nat) :
    startedMs + armMs + renewalPeriodMs ≤ renewalStartMs startedMs armMs := Nat.le_refl _

theorem a_slow_arm_does_not_make_renewal_due (startedMs armMs : Nat) :
    startedMs + armMs < renewalStartMs startedMs armMs := by
  unfold renewalStartMs renewalPeriodMs; omega

/-- What the user sees for a runtime connection. `waiting` states progress
without the user: the source is connecting or reconnecting, or it retries an
outage on `reconnectDelayMs`. `blocked` names a cause the user may need to act
on: a peer with no endpoint, which nothing in the client can connect, or a
runtime that refuses this client's credentials, which only its owner can
change (the source keeps reconnecting, so restored access needs no action
here). -/
inductive Tone where
  | ready | waiting | blocked
  deriving DecidableEq, Repr

inductive Reason where
  | connected | connecting | disconnected | refused | noEndpoint
  deriving DecidableEq, Repr

/-- `none` is a peer the observer has not yet reconciled. The desktop bridge
never withholds a reconciled status from a viewer: admission reads it
(`blocksAdmission`), so a hidden status would block the composer forever. -/
def presentation : Option Status → Tone × Reason
  | none | some .connecting => (.waiting, .connecting)
  | some .connected => (.ready, .connected)
  | some .disconnected => (.waiting, .disconnected)
  | some .refused => (.blocked, .refused)
  | some .noEndpoint => (.blocked, .noEndpoint)

/-- A served runtime held steady while the observer runs. `granted` is
whether the User's grant stands; the served runtime answers a revoked User by
omitting what it may not read. `refuses` is a runtime whose access owner
refuses this client's every request (HTTP 401/403): the client is not enrolled
there, its key was rotated, or a runtime that refuses at the handshake removed
its grant. -/
structure Served where
  reachable : Bool
  granted : Bool
  refuses : Bool
  deriving DecidableEq, Repr

def Served.read (served : Served) : SnapshotOutcome :=
  if !served.reachable then .unreachable else if served.refuses then .refused else .succeeded

/-- The event the observer next produces by itself at `status` against a
steady runtime, or `none` when only an outside change moves it. A `connecting`
or `refused` source (re)arms and reads a snapshot. A disconnected source
reconnects after `reconnectDelayMs`. A connected source sees a lost runtime as
a stream failure and a refusing one as a refused renewal; a quiet or empty
view is never a reason to reconnect. -/
def sourceEvent (served : Served) : Status → Option Event
  | .connecting | .refused =>
    if served.reachable then some (snapshotEvent served.read) else some .transportFailed
  | .disconnected =>
    some (if !served.reachable then .transportFailed
      else if served.refuses then .refused else .subscriptionsReady)
  | .connected =>
    if !served.reachable then some .transportFailed
    else if served.refuses then some .refused else none
  | .noEndpoint => none

def step (served : Served) (status : Status) : Status :=
  match sourceEvent served status with
  | some event => observe status event
  | none => status

def run (served : Served) : Nat → Status → Status
  | 0, status => status
  | n + 1, status => run served n (step served status)

theorem run_succ (served : Served) (n : Nat) (status : Status) :
    run served (n + 1) status = run served n (step served status) := rfl

theorem run_add (served : Served) (m n : Nat) (status : Status) :
    run served (m + n) status = run served n (run served m status) := by
  induction m generalizing status with
  | zero => simp [run]
  | succ m ih => rw [Nat.add_right_comm, run_succ, run_succ, ih]

/-- A reachable runtime, whether or not it still grants the User: every
status with an endpoint is `connected` within two source steps, and stays
there. A revoked User sees a connected view with nothing visible. -/
theorem reachable_settles_connected (granted : Bool) (status : Status)
    (h : status ≠ .noEndpoint) (n : Nat) :
    run ⟨true, granted, false⟩ (n + 2) status = .connected := by
  have fixed : ∀ k, run ⟨true, granted, false⟩ k .connected = .connected := by
    intro k; induction k with
    | zero => rfl
    | succ k ih => rw [run_succ]; simpa [step, sourceEvent] using ih
  rw [Nat.add_comm, run_add]
  cases status <;> simp_all [run, step, sourceEvent, observe, snapshotEvent, Served.read]

/-- A revoked User's connected view at a reachable runtime produces no event:
no reconnect, no resnapshot loop. -/
theorem quiet_view_never_reconnects (granted : Bool) :
    sourceEvent ⟨true, granted, false⟩ .connected = none := by
  cases granted <;> rfl

/-- An unreachable runtime: every status with an endpoint is `disconnected`
within two source steps and stays there, reconnecting, until the runtime
answers. -/
theorem unreachable_settles_disconnected (granted refuses : Bool) (status : Status)
    (h : status ≠ .noEndpoint) (n : Nat) :
    run ⟨false, granted, refuses⟩ (n + 2) status = .disconnected := by
  have fixed : ∀ k, run ⟨false, granted, refuses⟩ k .disconnected = .disconnected := by
    intro k; induction k with
    | zero => rfl
    | succ k ih => rw [run_succ]; simpa [step, sourceEvent, observe] using ih
  rw [Nat.add_comm, run_add]
  cases status <;> simp_all [run, step, sourceEvent, observe, snapshotEvent, Served.read]

/-- A reachable runtime that refuses this client: every status with an
endpoint is `refused` within two source steps and stays there, reconnecting on
the backoff, until the runtime accepts the client. -/
theorem refusing_settles_refused (granted : Bool) (status : Status)
    (h : status ≠ .noEndpoint) (n : Nat) :
    run ⟨true, granted, true⟩ (n + 2) status = .refused := by
  have fixed : ∀ k, run ⟨true, granted, true⟩ k .refused = .refused := by
    intro k; induction k with
    | zero => rfl
    | succ k ih =>
      rw [run_succ]; simpa [step, sourceEvent, observe, snapshotEvent, Served.read] using ih
  rw [Nat.add_comm, run_add]
  cases status <;> simp_all [run, step, sourceEvent, observe, snapshotEvent, Served.read]

/-- What a steady runtime settles into reads as its cause: `ready` exactly for
a reachable runtime that accepts the client, whatever the User may read there;
`waiting` with `disconnected` only for an unreachable one; and `blocked` with
`refused` for one that refuses the client, which is never a silent
`connecting`. A revoked User of the served runtime is never shown an outage or
a refusal. -/
theorem settled_presentation_names_the_cause (granted refuses : Bool) (status : Status)
    (h : status ≠ .noEndpoint) (n : Nat) :
    presentation (some (run ⟨true, granted, false⟩ (n + 2) status)) = (.ready, .connected) ∧
    presentation (some (run ⟨false, granted, refuses⟩ (n + 2) status)) =
      (.waiting, .disconnected) ∧
    presentation (some (run ⟨true, granted, true⟩ (n + 2) status)) = (.blocked, .refused) := by
  refine ⟨?_, ?_, ?_⟩
  · rw [reachable_settles_connected granted status h n]; rfl
  · rw [unreachable_settles_disconnected granted refuses status h n]; rfl
  · rw [refusing_settles_refused granted status h n]; rfl

/-- Distinct statuses name distinct reasons, so an outage never reads as a
connection attempt and a missing endpoint never reads as either. -/
theorem presentation_reason_injective (a b : Status)
    (h : (presentation (some a)).2 = (presentation (some b)).2) : a = b := by
  cases a <;> cases b <;> simp_all [presentation]

theorem ready_is_exactly_connected (status : Status) :
    (presentation (some status)).1 = .ready ↔ status = .connected := by
  cases status <;> simp [presentation]

theorem admission_blocked_unless_ready (status : Status) :
    blocksAdmission status = true ↔ (presentation (some status)).1 ≠ .ready := by
  cases status <;> simp [blocksAdmission, presentation]

end RuntimeConnectionObservation

/-! The observer's per-node load loop and what it shows the user. One loop owns
every node: a queued node starts one load, which captures the store fence,
reads the runtime and publishes into the node's own store through
`ClientDataPublication.reload`. Liveness of the loop (EventualPublication, Convergence) is checked by the
`ObserverResync` TLA model; the theorems here are the per-step safety and
ownership facts it relies on. -/
namespace ObserverResync

open ClientDataPublication
open RuntimeConnectionObservation (Status)

/-- How a finished snapshot load settles. -/
inductive Settlement where
  | published
  /-- A publication into the node's store since the capture rejected it. The store already shows newer rows; the next load needs no
  back-off. -/
  | superseded
  | transportFailed
  /-- The access owner refused the read; the source reconnects with fresh
  credentials (`RuntimeConnectionObservation.refused`). -/
  | refused
  deriving DecidableEq, Repr

inductive LoadResult where
  | rows | transportFailed | refused
  deriving DecidableEq, Repr

def settlement (s : State) (read : Read) : LoadResult → Settlement
  | .rows => if acceptsReload s read then .published else .superseded
  | .transportFailed => .transportFailed
  | .refused => .refused

structure Slot where
  pending : Bool
  /-- The retry timer must fire before the next load (transport back-off). -/
  waiting : Bool
  loading : Bool
  /-- The node's current source has armed every root. While it has not, the
  source's transport task owns progress: it retries the subscription after
  its reconnect delay until every root arms (`observe/remote.rs`; this
  premise is outside the model). -/
  armed : Bool
  status : Status
  deriving DecidableEq, Repr

def initial : Slot :=
  { pending := false, waiting := false, loading := false, armed := false, status := .connecting }

/-- `SourceStarted`: a new source for the node, configured, replaced or
reconnected. The previous source's load is aborted. -/
def sourceStarted (n : Slot) : Slot :=
  { n with loading := false, armed := false, status := .connecting }

/-- `SubscriptionsReady`: every root is armed, so the snapshot may start. A
refused slot stays refused until a read is accepted
(`RuntimeConnectionObservation.observe`). -/
def armed (n : Slot) : Slot :=
  { n with pending := true, armed := true,
           status := if n.status = .refused then .refused else .connecting }

/-- A delivered event, a direct refresh the fence rejected, or an explicit
resync request queues the node; queued requests coalesce into one load. -/
def queue (n : Slot) : Slot := { n with pending := true }

/-- A load starts only on an armed source. A refused slot is unarmed until its
reconnect arms it. -/
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
  | .refused =>
      { n with loading := false, pending := false, waiting := false, armed := false,
               status := .refused }

/-- A refusal the stream reports settles the slot as a refused load does. -/
def streamRefused (n : Slot) : Slot := settle n .refused

/-- What the user sees for a node, and why it is not stuck. -/
inductive Presentation where
  /-- Rows are being loaded, a load is queued, or the source is arming. -/
  | catchingUp
  | current
  /-- Cause: the runtime is unreachable. Next: the retry timer or the
  transport's re-arm, then a load. -/
  | retrying
  /-- Cause: the runtime refused this client's credentials. Next: the source's
  reconnect on the backoff re-arms and reads again; the user may ask the
  runtime's owner to restore access. -/
  | refused
  /-- Cause: the directory has no operator endpoint for the peer, so it has no
  source. Next: a reconcile that supplies one starts a source
  (`RuntimeConnectionObservation`). -/
  | noEndpoint
  /-- Nothing owns progress. No `Owned` slot presents this. -/
  | stalled
  deriving DecidableEq, Repr

def present (n : Slot) : Presentation :=
  match n.status with
  | .noEndpoint => .noEndpoint
  | .disconnected =>
      if n.pending || n.waiting || n.loading || !n.armed then .retrying else .stalled
  | .connecting => if n.pending || n.loading || !n.armed then .catchingUp else .stalled
  | .refused => if n.pending || n.loading || !n.armed then .refused else .stalled
  | .connected => if n.pending || n.loading then .catchingUp else .current

/-- Every connecting or disconnected slot has a queued or running load or an
unarmed source whose transport task owns the re-arm, and a waiting slot is
queued: the user-visible state is progressing or names its cause. -/
def Owned (n : Slot) : Prop :=
  (n.waiting = true → n.pending = true) ∧
    (n.status = .connecting → n.pending = true ∨ n.loading = true ∨ n.armed = false) ∧
    (n.status = .disconnected → n.pending = true ∨ n.loading = true ∨ n.armed = false) ∧
    (n.status = .refused → n.pending = true ∨ n.loading = true ∨ n.armed = false)

theorem owned_never_stalls (n : Slot) (h : Owned n) : present n ≠ .stalled := by
  obtain ⟨_, hConnecting, hDisconnected, hRefused⟩ := h
  cases n with
  | mk pending waiting loading armed status =>
    cases status <;> cases pending <;> cases waiting <;> cases loading <;> cases armed <;>
      simp_all [present]

/-- An owned connecting or disconnected slot names its next step: a load can
start now, the retry timer is due, a load is running, or the source is
arming. -/
theorem owned_names_next_step (n : Slot) (h : Owned n)
    (hs : n.status = .connecting ∨ n.status = .disconnected ∨ n.status = .refused) :
    startable n = true ∨ n.waiting = true ∨ n.loading = true ∨ n.armed = false := by
  obtain ⟨hWaiting, hConnecting, hDisconnected, hRefused⟩ := h
  cases n with
  | mk pending waiting loading armed status =>
    rcases hs with hs | hs | hs <;> subst hs <;>
      cases pending <;> cases waiting <;> cases loading <;> cases armed <;>
      simp_all [startable]

theorem initial_owned : Owned initial := by
  simp [Owned, initial]

theorem source_started_owned (n : Slot) (h : Owned n) : Owned (sourceStarted n) := by
  cases n; simp_all [Owned, sourceStarted]

theorem armed_owned (n : Slot) : Owned (armed n) := by
  simp only [Owned, armed]
  split <;> simp

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

theorem stream_refused_owned (n : Slot) (h : Owned n) : Owned (streamRefused n) :=
  settle_owned n h .refused

theorem timer_owned (n : Slot) (h : Owned n) : Owned (timer n) := by
  cases n; simp_all [Owned, timer]

/-- A dropped stream keeps a queued load from starting until the source
re-arms; the re-arm queues it and it starts unless the retry timer is due. -/
theorem stream_failure_waits_for_arm (n : Slot) :
    startable (streamFailed n) = false ∧
      startable (armed (streamFailed n)) = !n.waiting := by
  simp [startable, streamFailed, armed]

theorem refused_never_starts (n : Slot) : startable (streamRefused n) = false := by
  simp [startable, streamRefused, settle]

/-- A refused slot names the refusal while its reconnect is pending. -/
theorem refused_is_named (n : Slot) : present (streamRefused n) = .refused := by
  simp [present, streamRefused, settle]

/-- A snapshot that failed for a transport reason keeps the source armed: the
read is retried after the timer, the streams are not torn down. -/
theorem transport_failed_load_keeps_armed (n : Slot) :
    (settle n .transportFailed).armed = n.armed := by
  simp [settle]

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
request events publishes and shows the node connected. -/
theorem request_events_never_supersede (s : State) (events : Nat) (n : Slot) :
    settlement (requestEvents s events) (beginRead s) .rows = .published ∧
      (settle n (settlement (requestEvents s events) (beginRead s) .rows)).status =
        .connected := by
  have h := arbitrarily_many_request_events_preserve_reload s events
  simp [settlement, h, settle]

/-- NoStaleCommit through the loop: a publication into the node's store since
capture supersedes the load, and the node is queued for a fresh read. -/
theorem publication_supersedes_and_requeues (s : State) (n : Slot) :
    settlement (dataChange s) (beginRead s) .rows = .superseded ∧
      (settle n (settlement (dataChange s) (beginRead s) .rows)).pending = true := by
  have h := publication_rejects_reload s
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
