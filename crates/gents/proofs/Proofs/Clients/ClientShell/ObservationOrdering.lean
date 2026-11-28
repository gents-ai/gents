import Proofs.Clients.ClientShell.Types
import Proofs.Store.AccessClassification

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
after restart. Enrollment authority and send readiness remain with their
existing owners; operator configuration access is not required. Unreadable
bootstrap state stays unresolved, and an active wizard retains ownership until
explicit completion.

`localHomeLost` is true when this launch retired the saved peer of a local node
whose home is gone. Saved remotes do not complete setup then: the user approved
that a home that disappeared returns to the setup wizard, which still offers
connecting to a remote, rather than opening without the local node. -/
def observeFirstRun (phase : FirstRunPhase) (snapshotReady : Bool)
    (initConfigStatus : InitConfigStatus)
    (needsSetup authoritative localRuntimeAllowed remoteConfigured localHomeLost : Bool) :
    FirstRunPhase :=
  match phase with
  | .active => .active
  | .done => .done
  | .unknown =>
      if !snapshotReady then .unknown
      else match initConfigStatus with
        | .unreadable | .unknown => .unknown
        | status =>
          if !localRuntimeAllowed || status == .absent then
            if remoteConfigured && !localHomeLost then .done else .active
          else if !authoritative then .unknown
          else if needsSetup then .active else .done

theorem configured_remote_completes_without_operator_access :
    observeFirstRun .unknown true .absent true false false true false = .done := by rfl

theorem unconfigured_remote_keeps_setup :
    observeFirstRun .unknown true .absent false true false false false = .active := by rfl

theorem desktop_saved_remote_completes_without_local_home (needsSetup authoritative : Bool) :
    observeFirstRun .unknown true .absent needsSetup authoritative true true false = .done := by
  cases needsSetup <;> cases authoritative <;> rfl

theorem desktop_without_local_home_or_remote_keeps_setup
    (needsSetup authoritative localHomeLost : Bool) :
    observeFirstRun .unknown true .absent needsSetup authoritative true false localHomeLost
      = .active := by
  cases needsSetup <;> cases authoritative <;> cases localHomeLost <;> rfl

theorem lost_local_home_returns_to_setup_despite_saved_remotes
    (needsSetup authoritative remoteConfigured : Bool) :
    observeFirstRun .unknown true .absent needsSetup authoritative true remoteConfigured true
      = .active := by
  cases needsSetup <;> cases authoritative <;> cases remoteConfigured <;> rfl

theorem active_setup_keeps_ownership (snapshotReady : Bool) (status : InitConfigStatus)
    (needsSetup authoritative localRuntimeAllowed remoteConfigured localHomeLost : Bool) :
    observeFirstRun .active snapshotReady status needsSetup authoritative localRuntimeAllowed
      remoteConfigured localHomeLost = .active := by
  rfl

theorem unreadable_bootstrap_stays_unresolved
    (needsSetup authoritative localRuntimeAllowed remoteConfigured localHomeLost : Bool) :
    observeFirstRun .unknown true .unreadable needsSetup authoritative localRuntimeAllowed
      remoteConfigured localHomeLost = .unknown := by
  rfl

/-- A client observation can recover a client error, not managed-server authority.

A snapshot is this device's saved settings, read. A configuration error says
they did not arrive (their read failed, or ran past startup's bound), so a
snapshot recovers it: the person is not left on an error once the settings
are in hand. It moves the error where it moves loading, to the client's start
or to ready, never back to loading, since the settings it carries are read.

`autostartDeclined` is true exactly when the desktop will not autostart the
client. The shell derives it from the one predicate that also gates autostart
(`shouldAutoStartDesktopClient`), so startup never waits on a client that
nothing will start. -/
def observeStartup (phase : StartupPhase) (running autostartDeclined : Bool) :
    StartupPhase :=
  match phase with
  | .loadingConfiguration | .startingClient | .configurationError =>
      if running || autostartDeclined then .ready else .startingClient
  | .clientError => if running then .ready else .clientError
  | other => other

theorem observed_settings_recover_configuration_error (running autostartDeclined : Bool) :
    observeStartup .configurationError running autostartDeclined =
      observeStartup .loadingConfiguration running autostartDeclined := by rfl

theorem observed_running_client_recovers_client_error (autostartDeclined : Bool) :
    observeStartup .clientError true autostartDeclined = .ready := by rfl

theorem stopped_client_does_not_clear_client_error (autostartDeclined : Bool) :
    observeStartup .clientError false autostartDeclined = .clientError := by rfl

theorem client_observation_does_not_clear_managed_error (running autostartDeclined : Bool) :
    observeStartup .managedServerError running autostartDeclined = .managedServerError := by rfl

/-- What a ready app shows: the shell, or a screen that asks something of the
person. Waiting is never one of them. -/
inductive StartupGate where
  | shell | unreadable | notConnected
  deriving DecidableEq, Repr

/-- Once this device's own saved settings are read, the shell shows. A first
run still unsettled because the node's configuration has not been read from
the node yet waits in the shell, with the node shown connecting: that read
needs the node, which may be slow, stopped or away. Only settings that cannot
be read, or a configured node this desktop is not connected to, take the
window instead, each with what to do about it. `nodeNotConnected` is the
connect screen's verdict on the local node (`DesktopConnect`). -/
def startupGate (settled : Bool) (initConfigStatus : InitConfigStatus)
    (nodeNotConnected : Bool) : StartupGate :=
  if settled then .shell
  else match initConfigStatus with
    | .unreadable => .unreadable
    | .configured => if nodeNotConnected then .notConnected else .shell
    | _ => .shell

theorem a_settled_first_run_shows_the_shell (status : InitConfigStatus) (notConnected : Bool) :
    startupGate true status notConnected = .shell := by rfl

/-- A node that is slow or away, but known, never holds the window. -/
theorem a_connected_configured_node_waits_in_the_shell :
    startupGate false .configured false = .shell := by rfl

/-- Only unreadable settings and a configured node this desktop is not
connected to take the window. -/
theorem the_window_is_taken_only_to_ask (settled : Bool) (status : InitConfigStatus)
    (notConnected : Bool) (h : startupGate settled status notConnected ≠ .shell) :
    settled = false ∧
      (status = .unreadable ∨ (status = .configured ∧ notConnected = true)) := by
  cases settled <;> cases status <;> cases notConnected <;> simp_all [startupGate]

/-- How a read of this device's saved settings answered: with the snapshot,
or with an error. -/
inductive SettingsAnswer where
  | succeeded | failed
  deriving DecidableEq, Repr

/-- The startup phase once a read of this device's saved settings answers.
Every read begins an attempt; `current` is true when no read began after it
(the snapshot publication's generation, `ClientObservationOrdering.accepts`).
A superseded attempt's answer moves nothing. A current success is a snapshot
observation (`observeStartup`), with `running` and `autostartDeclined` taken
from the snapshot it carries. A current failure ends the startup step that
waits on it: loading becomes a configuration error and a client start a
client error. A failure moves no other phase, so a ready app stays ready and
an error stays that error. -/
def settingsAnswered (phase : StartupPhase) (current : Bool) (answer : SettingsAnswer)
    (running autostartDeclined : Bool) : StartupPhase :=
  if current then
    match answer with
    | .succeeded => observeStartup phase running autostartDeclined
    | .failed =>
        match phase with
        | .loadingConfiguration => .configurationError
        | .startingClient => .clientError
        | other => other
  else phase

/-- Startup bounds its wait on the current settings read with a client clock.
Past the bound, a startup still loading on that read shows a configuration
error with its retry, and nothing else moves. The read is not abandoned: its
answer still arrives through `settingsAnswered`, and a success recovers. -/
def settingsReadTimedOut (phase : StartupPhase) (current : Bool) : StartupPhase :=
  match current, phase with
  | true, .loadingConfiguration => .configurationError
  | _, other => other

/-- A late success recovers a configuration error to where the settings it
carries warrant, never to loading. -/
theorem current_settings_recover_configuration_error (running autostartDeclined : Bool) :
    settingsAnswered .configurationError true .succeeded running autostartDeclined =
      if running || autostartDeclined then .ready else .startingClient := by rfl

theorem superseded_answer_moves_nothing (phase : StartupPhase) (answer : SettingsAnswer)
    (running autostartDeclined : Bool) :
    settingsAnswered phase false answer running autostartDeclined = phase := by rfl

theorem superseded_timeout_moves_nothing (phase : StartupPhase) :
    settingsReadTimedOut phase false = phase := by cases phase <;> rfl

theorem late_failure_leaves_a_ready_app (current running autostartDeclined : Bool) :
    settingsAnswered .ready current .failed running autostartDeclined = .ready := by
  cases current <;> rfl

theorem failure_keeps_configuration_error (current running autostartDeclined : Bool) :
    settingsAnswered .configurationError current .failed running autostartDeclined =
      .configurationError := by
  cases current <;> rfl

/-- One launch's startup as the settings reads drive it: the phase, the
generation of the latest read, and whether a current read has answered with
the settings. `retry` is Try again (or skipping the managed node's check):
startup loads again and a new read begins. `readBegins` is any other read,
which supersedes the one before it. -/
structure Launch where
  phase : StartupPhase
  generation : Nat
  settingsRead : Bool

inductive LaunchEvent where
  | retry
  | readBegins
  | timedOut (captured : Nat)
  | answered (captured : Nat) (answer : SettingsAnswer) (running autostartDeclined : Bool)

def Launch.initial : Launch :=
  { phase := .checkingManagedServer, generation := 0, settingsRead := false }

def Launch.step (s : Launch) : LaunchEvent → Launch
  | .retry => { s with phase := .loadingConfiguration, generation := s.generation + 1 }
  | .readBegins => { s with generation := s.generation + 1 }
  | .timedOut captured =>
      { s with phase := settingsReadTimedOut s.phase (accepts s.generation captured) }
  | .answered captured answer running autostartDeclined =>
      let current := accepts s.generation captured
      { s with
        phase := settingsAnswered s.phase current answer running autostartDeclined
        settingsRead := s.settingsRead || (current && answer == .succeeded) }

inductive Launch.Reachable : Launch → Prop
  | initial : Launch.Reachable Launch.initial
  | step {s : Launch} (event : LaunchEvent) :
      Launch.Reachable s → Launch.Reachable (s.step event)

private theorem answered_ready (phase : StartupPhase) (current : Bool)
    (answer : SettingsAnswer) (running autostartDeclined : Bool)
    (h : settingsAnswered phase current answer running autostartDeclined = .ready) :
    phase = .ready ∨ (current = true ∧ answer = .succeeded) := by
  cases current <;> cases answer <;> cases phase <;> simp_all [settingsAnswered]

private theorem timed_out_ready (phase : StartupPhase) (current : Bool)
    (h : settingsReadTimedOut phase current = .ready) : phase = .ready := by
  cases current <;> cases phase <;> simp_all [settingsReadTimedOut]

/-- The shell never shows before this device's saved settings are read: a
launch is ready only once a current read has answered with them. -/
theorem ready_only_after_settings_read (s : Launch) (reachable : s.Reachable)
    (ready : s.phase = .ready) : s.settingsRead = true := by
  induction reachable with
  | initial => simp [Launch.initial] at ready
  | step event _ ih =>
      cases event with
      | retry => simp [Launch.step] at ready
      | readBegins => exact ih ready
      | timedOut captured => exact ih (timed_out_ready _ _ ready)
      | answered captured answer running autostartDeclined =>
          rcases answered_ready _ _ _ _ _ ready with was | ⟨current, success⟩
          · simp [Launch.step, ih was]
          · simp [Launch.step, current, success]

/-- Only a retry puts startup back to loading: no answer and no timeout move
an error, or anything else, to loading. -/
theorem only_a_retry_reloads (s : Launch) (event : LaunchEvent) (notRetry : event ≠ .retry)
    (loading : (s.step event).phase = .loadingConfiguration) :
    s.phase = .loadingConfiguration := by
  cases event with
  | retry => exact absurd rfl notRetry
  | readBegins => exact loading
  | timedOut captured =>
      simp only [Launch.step] at loading
      generalize accepts s.generation captured = current at loading
      cases current <;> cases h : s.phase <;> simp_all [settingsReadTimedOut]
  | answered captured answer running autostartDeclined =>
      simp only [Launch.step] at loading
      generalize accepts s.generation captured = current at loading
      cases current <;> cases answer <;> cases h : s.phase <;>
        simp_all [settingsAnswered, observeStartup] <;>
        split at loading <;> simp_all

/-- A configuration error is entered only from loading on the current read,
and left only by a retry or by a current read answering with the settings. -/
theorem configuration_error_moves_only_on_settings_or_retry (s : Launch) (event : LaunchEvent)
    (error : s.phase = .configurationError)
    (moved : (s.step event).phase ≠ .configurationError) :
    event = .retry ∨ ∃ running autostartDeclined,
      event = .answered s.generation .succeeded running autostartDeclined := by
  cases event with
  | retry => exact Or.inl rfl
  | readBegins => exact absurd error moved
  | timedOut captured =>
      simp only [Launch.step, error] at moved
      cases accepts s.generation captured <;> simp [settingsReadTimedOut] at moved
  | answered captured answer running autostartDeclined =>
      simp only [Launch.step, error] at moved
      by_cases current : accepts s.generation captured = true
      · cases answer
        · refine Or.inr ⟨running, autostartDeclined, ?_⟩
          simp [accepts] at current
          simp [current]
        · simp [current, settingsAnswered] at moved
      · simp [current, settingsAnswered] at moved

theorem configuration_error_entered_only_from_loading (s : Launch) (event : LaunchEvent)
    (entered : (s.step event).phase = .configurationError) :
    s.phase = .configurationError ∨ s.phase = .loadingConfiguration := by
  cases event with
  | retry => simp [Launch.step] at entered
  | readBegins => exact Or.inl entered
  | timedOut captured =>
      simp only [Launch.step] at entered
      generalize accepts s.generation captured = current at entered
      cases current <;> cases h : s.phase <;> simp_all [settingsReadTimedOut]
  | answered captured answer running autostartDeclined =>
      simp only [Launch.step] at entered
      generalize accepts s.generation captured = current at entered
      cases current <;> cases answer <;> cases h : s.phase <;>
        simp_all [settingsAnswered, observeStartup] <;>
        split at entered <;> simp_all

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

/-- The patch decision for the whole store: any publication and any request
event since the capture rejects. It decides a patch with a row that names no
session scope; `PatchFence.accepts` decides one that names them. -/
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

/-- The store after a capture saw a publication into it, a request event of
it, both or neither. -/
def sinceCapture (s : State) (publication event : Bool) : State :=
  let published := if publication then dataChange s else s
  if event then requestEvent published else published

/-- The reload fence as a finite decision: whether a reload captured before
`sinceCapture` still publishes. -/
def reloadAcceptedSince (publication event : Bool) : Bool :=
  let start : State :=
    { dataEpoch := 0, provenanceEpoch := 0, requestEventEpoch := 0, transcriptEpoch := 0 }
  acceptsReload (sinceCapture start publication event) (beginRead start)

/-- The finite table decides every store, whatever its epochs: `ObserverResync`
TLA reads its reload fence from it. -/
theorem reload_accepted_since_any_state (s : State) (publication event : Bool) :
    acceptsReload (sinceCapture s publication event) (beginRead s) =
      reloadAcceptedSince publication event := by
  cases publication <;> cases event <;>
    simp [reloadAcceptedSince, sinceCapture, acceptsReload, dataChange, requestEvent, beginRead]

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

/-! ### The patch fence, per session scope

A patch merges the rows it read into the store, so it must not publish a row
read before a change the store was told about or already shows. `accepts`
decides that for the whole store: any request event and any publication since
the capture rejects the patch. On a node where other sessions are active that
rejects nearly every patch of a quiet session, although none of its rows moved.

A patch carries rows of known session scopes: the scopes its sessions and
requests name (the selected session and the scopes its request closure
reaches). Every step that moves `accepts` is classified by what it can have
changed for a patch (`reach`), and the fence remembers the step each scope and
the whole store last moved at. A patch is rejected only if the whole store, or
a scope it carries, moved after its capture. -/

/-- What a step of the store can have changed for a patch. -/
inductive Reach where
  /-- Rows no patch carries. -/
  | nothing
  /-- Rows of the session scopes the step names. -/
  | named
  /-- Rows of any scope. -/
  | whole
  deriving DecidableEq, Repr

/-- The steps that move `accepts`: request events and publications. -/
inductive PatchStep where
  /-- A request or session change whose event names its session scope. -/
  | scopeEvent
  /-- A change event that names no session scope (the Node, a trigger, a
  deletion, a trigger-caused request) or a gap in the subscription. -/
  | unscopedEvent
  /-- A change of a collection whose rows no patch carries. -/
  | collectionEvent
  /-- A change of a collection whose rows patches carry (goals). -/
  | carriedCollectionEvent
  /-- A tool-call change (`provenanceInvalidation`): its event names only the
  tool call, not the requests whose lineage it moves. -/
  | provenanceEvent
  /-- A reload of the whole node. -/
  | nodeReload
  /-- A reload of session scopes. -/
  | scopeReload
  /-- A reload of collections whose rows no patch carries. -/
  | collectionReload
  /-- A reload of a collection whose rows patches carry. -/
  | carriedCollectionReload
  /-- An accepted patch: it publishes rows of the scopes it carries. -/
  | patch
  /-- An accepted patch with a row that names no scope. -/
  | unscopedPatch
  deriving DecidableEq, Repr

/-- Conformance function. -/
def reach : PatchStep → Reach
  | .scopeEvent => .named
  | .unscopedEvent => .whole
  | .collectionEvent => .nothing
  | .carriedCollectionEvent => .whole
  | .provenanceEvent => .whole
  | .nodeReload => .whole
  | .scopeReload => .named
  | .collectionReload => .nothing
  | .carriedCollectionReload => .whole
  | .patch => .named
  | .unscopedPatch => .whole

/-- `sequence` counts the steps; `whole` and `scope k` are the steps at which
the whole store and scope `k` last moved. -/
structure PatchFence where
  sequence : Nat
  whole : Nat
  scope : Nat → Nat

def PatchFence.initial : PatchFence :=
  { sequence := 0, whole := 0, scope := fun _ => 0 }

def PatchFence.WellFormed (f : PatchFence) : Prop :=
  f.whole ≤ f.sequence ∧ ∀ k, f.scope k ≤ f.sequence

/-- A patch read captures the step count before it reads. -/
def PatchFence.capture (f : PatchFence) : Nat := f.sequence

/-- One step of kind `reach` naming the scopes `named`. -/
def PatchFence.move (f : PatchFence) (reach : Reach) (named : List Nat) : PatchFence :=
  match reach with
  | .nothing => { f with sequence := f.sequence + 1 }
  | .named =>
      { sequence := f.sequence + 1
        whole := f.whole
        scope := fun k => if k ∈ named then f.sequence + 1 else f.scope k }
  | .whole => { f with sequence := f.sequence + 1, whole := f.sequence + 1 }

/-- Whether a patch captured at `captured` carrying the scopes `carried` may
publish. A patch with a row that names no scope (`none`) is decided for the
whole store, as `accepts` decides it: any step since the capture rejects it. -/
def PatchFence.accepts (f : PatchFence) (captured : Nat) : Option (List Nat) → Bool
  | none => decide (f.sequence = captured)
  | some carried =>
      decide (f.whole ≤ captured) && carried.all fun k => decide (f.scope k ≤ captured)

theorem PatchFence.initial_well_formed : PatchFence.initial.WellFormed := by
  simp [PatchFence.initial, PatchFence.WellFormed]

theorem PatchFence.move_well_formed (f : PatchFence) (h : f.WellFormed) (reach : Reach)
    (named : List Nat) : (f.move reach named).WellFormed := by
  obtain ⟨hw, hs⟩ := h
  cases reach
  · exact ⟨by simp [PatchFence.move]; omega, fun k => by have := hs k; simp [PatchFence.move]; omega⟩
  · refine ⟨by simp [PatchFence.move]; omega, fun k => ?_⟩
    have := hs k
    by_cases hk : k ∈ named <;> simp [PatchFence.move, hk]
    omega
  · exact ⟨by simp [PatchFence.move], fun k => by have := hs k; simp [PatchFence.move]; omega⟩

/-- A patch published before any further step is accepted. -/
theorem PatchFence.unmoved_patch_is_accepted (f : PatchFence) (h : f.WellFormed)
    (carried : Option (List Nat)) : f.accepts f.capture carried = true := by
  obtain ⟨hw, hs⟩ := h
  cases carried with
  | none => simp [PatchFence.accepts, PatchFence.capture]
  | some carried =>
      simp only [PatchFence.accepts, PatchFence.capture, Bool.and_eq_true, decide_eq_true_eq,
        List.all_eq_true]
      exact ⟨hw, fun k _ => hs k⟩

/-- Whatever the store-wide rule accepts, the scoped rule accepts. -/
theorem PatchFence.scoped_accepts_what_whole_store_accepts (f : PatchFence) (h : f.WellFormed)
    (captured : Nat) (carried : List Nat) (whole : f.accepts captured none = true) :
    f.accepts captured (some carried) = true := by
  have hc : f.sequence = captured := by simpa [PatchFence.accepts] using whole
  subst hc
  exact f.unmoved_patch_is_accepted h (some carried)

/-- A step that names only other scopes leaves the patch's decision as it was:
another session's request events, scope reloads and patches do not reject it. -/
theorem PatchFence.other_scopes_preserve_patch (f : PatchFence) (captured : Nat)
    (named carried : List Nat) (disjoint : ∀ k ∈ carried, k ∉ named) :
    (f.move .named named).accepts captured (some carried) = f.accepts captured (some carried) := by
  simp only [PatchFence.accepts, PatchFence.move]
  rw [Bool.eq_iff_iff]
  simp only [Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true]
  constructor
  · rintro ⟨hw, hs⟩
    exact ⟨hw, fun k hk => by have := hs k hk; simpa [disjoint k hk] using this⟩
  · rintro ⟨hw, hs⟩
    exact ⟨hw, fun k hk => by simpa [disjoint k hk] using hs k hk⟩

/-- A step that reaches no patch row leaves every scoped patch's decision as it was. -/
theorem PatchFence.unreached_step_preserves_patch (f : PatchFence) (captured : Nat)
    (named carried : List Nat) :
    (f.move .nothing named).accepts captured (some carried) = f.accepts captured (some carried) := by
  simp [PatchFence.accepts, PatchFence.move]

/-- However many steps name only other scopes while a patch is read, it publishes. -/
theorem PatchFence.arbitrarily_many_other_scope_steps_preserve_patch (f : PatchFence)
    (h : f.WellFormed) (carried : List Nat) (steps : List (List Nat))
    (disjoint : ∀ named ∈ steps, ∀ k ∈ carried, k ∉ named) :
    (steps.foldl (fun fence named => fence.move .named named) f).accepts f.capture (some carried)
      = true := by
  suffices ∀ (g : PatchFence), g.accepts f.capture (some carried) = true →
      (steps.foldl (fun fence named => fence.move .named named) g).accepts f.capture
        (some carried) = true from this f (f.unmoved_patch_is_accepted h (some carried))
  induction steps with
  | nil => intro g hg; simpa using hg
  | cons named rest ih =>
      intro g hg
      simp only [List.foldl_cons]
      apply ih (fun n hn => disjoint n (List.mem_cons_of_mem _ hn))
      rw [PatchFence.other_scopes_preserve_patch g f.capture named carried
        (disjoint named (List.mem_cons_self ..))]
      exact hg

/-- A step naming a scope the patch carries rejects it: an event of that scope,
a reload of it, or another patch of it. -/
theorem PatchFence.own_scope_rejects_patch (f : PatchFence) (named carried : List Nat)
    (k : Nat) (carries : k ∈ carried) (moved : k ∈ named) :
    (f.move .named named).accepts f.capture (some carried) = false := by
  simp only [PatchFence.accepts, PatchFence.move, PatchFence.capture, Bool.and_eq_false_iff,
    decide_eq_false_iff_not, List.all_eq_false]
  right
  exact ⟨k, carries, by simp [moved]⟩

/-- A step that reaches the whole store rejects every patch read before it. -/
theorem PatchFence.whole_store_step_rejects_patch (f : PatchFence) (named : List Nat)
    (carried : Option (List Nat)) :
    (f.move .whole named).accepts f.capture carried = false := by
  cases carried with
  | none => simp [PatchFence.accepts, PatchFence.move, PatchFence.capture]
  | some carried =>
      simp only [PatchFence.accepts, PatchFence.move, PatchFence.capture, Bool.and_eq_false_iff,
        decide_eq_false_iff_not]
      left
      omega

/-- A patch with a row naming no scope is rejected by any step, as `accepts` rejects it. -/
theorem PatchFence.unscoped_patch_rejected_by_any_step (f : PatchFence) (reach : Reach)
    (named : List Nat) : (f.move reach named).accepts f.capture none = false := by
  cases reach <;> simp [PatchFence.accepts, PatchFence.move, PatchFence.capture]

/-- Every step moves the sequence: the fence counts exactly the steps that move `accepts`. -/
theorem PatchFence.every_step_counts (f : PatchFence) (reach : Reach) (named : List Nat) :
    (f.move reach named).sequence = f.sequence + 1 := by
  cases reach <;> rfl

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

`refused` means access to the selected Node is unavailable: the native access
owner refused the request, or the snapshot owner could not observe the
already identified Node root (`selectedRootSnapshot`). It does not infer why
access is unavailable. The source reconnects on `reconnectDelayMs`, so restored
access connects by itself. Empty configuration beneath a visible Node is an
ordinary connected view. -/
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

/-- Whether selecting a node reads it at once, given its observation (`none`
before the node is observed). While the observer is still connecting, its own
first read is what makes the node current; a second full read beside it only
collides with that one (one is discarded or read again), doubling a launch's
requests. In every other state, and before any observation, the selection
reads the node. -/
def selectionReadsNode (observed : Option Status) : Bool :=
  decide (observed ≠ some .connecting)

/-- `selectionReadsNode` over a flag for whether the node is observed and its
status, the shape its conformance cases take. -/
def selectionReads (observed : Bool) (status : Status) : Bool :=
  selectionReadsNode (if observed then some status else none)

theorem a_connecting_observer_defers_the_selection_read :
    selectionReadsNode (some .connecting) = false := by decide

theorem selection_reads_in_every_other_state (observed : Option Status)
    (other : observed ≠ some .connecting) : selectionReadsNode observed = true := by
  simp [selectionReadsNode, other]

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

/-- The stream end of a source whose stream or handshake failed with `error`,
as the source receives it: erased, and classified by the access owner
(`AccessClassification.Error.erased`). -/
def failedStreamEnd (error : AccessClassification.Error) : StreamEnd :=
  .failed (decide (error.erased = .refused))

/-- A failure the access owner classifies as a denial is published as
`refused`, whatever owners wrap the variant that holds it
(`AccessClassification.Error.classification_survives_every_wrapping`). -/
theorem classified_refusal_is_refused (holdings : List AccessClassification.Holding)
    (failure : AccessClassification.Failure) (refused : failure.classification = .refused) :
    streamEndEvent (failedStreamEnd (AccessClassification.Chain.wrapAll holdings
      (.wrapper .sourced (.owner failure)))) = some .refused := by
  simp [failedStreamEnd, AccessClassification.Error.classification_survives_every_wrapping,
    refused, streamEndEvent]

/-- The premise of `classified_refusal_is_refused`: a denial held by a
transparent variant is published as a transport failure. -/
theorem hidden_refusal_reads_as_an_outage :
    streamEndEvent (failedStreamEnd (.wrapper .transparent (.owner .forbidden)))
      = some .transportFailed := by
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

/-- How the selected Node snapshot ended. Native ACP may omit an unreadable
Node with HTTP 200 and no errors. The snapshot targets an already identified
Node: its missing root means this client cannot observe that Node, rather than
that the Node has no configuration. Refusal is a presentation of unavailable
access, not a claim about whether the grant was revoked or the Node removed.
Unrelated omitted collections remain ordinary empty lists. -/
inductive SnapshotOutcome where
  | succeeded | refused | unreachable
  deriving DecidableEq, Repr

def selectedRootSnapshot (visible : Bool) : SnapshotOutcome :=
  if visible then .succeeded else .refused

theorem missing_selected_root_is_not_connected :
    selectedRootSnapshot false = .refused := by rfl

def snapshotEvent : SnapshotOutcome → Event
  | .succeeded => .snapshotSucceeded
  | .refused => .refused
  | .unreachable => .transportFailed

/-- How a snapshot read that failed with `failure` ends. Every snapshot read
is a typed view read whose error the reader erases
(`AccessClassification.viewRead`). -/
def failedSnapshot (failure : AccessClassification.Failure) : SnapshotOutcome :=
  if (AccessClassification.viewRead failure).erased = .refused then .refused else .unreachable

theorem failed_snapshot_is_refused_iff_classified (failure : AccessClassification.Failure) :
    failedSnapshot failure = .refused ↔ failure.classification = .refused := by
  cases failure <;> decide

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

/-- A snapshot read the runtime refused reconnects its source; any other
failed read, a conflict included, keeps the streams and is retried. -/
theorem failed_snapshot_reconnects_iff_refused (failure : AccessClassification.Failure) :
    reconnects .snapshot (snapshotEvent (failedSnapshot failure)) = true
      ↔ failure.classification = .refused := by
  cases failure <;> decide

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

/-- How long a snapshot whose read failed for a transport reason waits before
it is read again while its source stays armed, given the node's consecutive
failed reads before this one (`snapshotFailuresAfter`). It is not a reconnect:
the streams are still armed. It waits on the reconnect schedule: a runtime
whose subscriptions stay up while every read fails would otherwise be read four
times a second for as long as it lasts, and shown as reconnecting on each read. -/
def snapshotRetryDelayMs (failures : Nat) : Nat := reconnectDelayMs failures

/-- The node's consecutive failed reads after a read that ended with
`outcome`. A read the runtime answered restarts the count, whether it published
or a newer publication superseded it; so does a refusal, whose source
reconnects on its own count (`reconnects`). -/
def snapshotFailuresAfter (failures : Nat) : SnapshotOutcome → Nat
  | .unreachable => failures + 1
  | .succeeded | .refused => 0

theorem snapshot_retry_positive (failures : Nat) : 0 < snapshotRetryDelayMs failures :=
  reconnect_delay_positive failures

theorem first_snapshot_retries_wait_250ms :
    (List.range 5).map snapshotRetryDelayMs = [250, 250, 250, 250, 250] := by decide

/-- A runtime whose every read fails is read again at most once per cap from
the twelfth consecutive failure on. -/
theorem failing_reads_back_off (failures : Nat) (h : 11 ≤ failures) :
    snapshotRetryDelayMs failures = reconnectCapMs :=
  reconnect_delay_reaches_the_cap failures h

theorem answered_read_restarts_the_retry_schedule (failures : Nat) :
    snapshotRetryDelayMs (snapshotFailuresAfter failures .succeeded) = reconnectBaseMs := by
  show reconnectDelayMs 0 = reconnectBaseMs
  decide

/-- The reconnect count and the failed-read count a node that is not
connected has after the user asks to reconnect it now (try again, a status
check, the window shown again, a runtime this desktop just started). Its source
restarts at once at the same endpoint, as a due reconnect restarts it, and its
read retry timer is cleared (`ObserverResync.retryNow`). The counts start over:
the request is the user's evidence that the runtime may be back, which is what
the backoff waits for, and a runtime still down is redialed after
`reconnectBaseMs` and backs off again. A connected node keeps its source and
counts. -/
def retryNowCount : Nat := 0

theorem retry_now_starts_the_schedules_over :
    reconnectDelayMs retryNowCount = reconnectBaseMs ∧
      snapshotRetryDelayMs retryNowCount = reconnectBaseMs := by
  decide

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
  if !served.reachable then .unreachable
  else if served.refuses then .refused else selectedRootSnapshot served.granted

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
      else if served.refuses || !served.granted then .refused else .subscriptionsReady)
  | .connected =>
    if !served.reachable then some .transportFailed
    else if served.refuses || !served.granted then some .refused else none
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

/-- A reachable Node whose root is visible settles connected. -/
theorem reachable_settles_connected (status : Status)
    (h : status ≠ .noEndpoint) (n : Nat) :
    run ⟨true, true, false⟩ (n + 2) status = .connected := by
  have fixed : ∀ k, run ⟨true, true, false⟩ k .connected = .connected := by
    intro k; induction k with
    | zero => rfl
    | succ k ih => rw [run_succ]; simpa [step, sourceEvent] using ih
  rw [Nat.add_comm, run_add]
  cases status <;> simp_all [run, step, sourceEvent, observe, snapshotEvent, Served.read,
    selectedRootSnapshot]

/-- A visible quiet Node needs no reconnect. -/
theorem quiet_view_never_reconnects :
    sourceEvent ⟨true, true, false⟩ .connected = none := by rfl

/-- Loss of the selected root reports unavailable access even when the
transport still accepts this client's subscriptions. -/
theorem hidden_node_settles_refused (status : Status)
    (h : status ≠ .noEndpoint) (n : Nat) :
    run ⟨true, false, false⟩ (n + 2) status = .refused := by
  have fixed : ∀ k, run ⟨true, false, false⟩ k .refused = .refused := by
    intro k; induction k with
    | zero => rfl
    | succ k ih =>
      rw [run_succ]; simpa [step, sourceEvent, observe, snapshotEvent, Served.read,
        selectedRootSnapshot] using ih
  rw [Nat.add_comm, run_add]
  cases status <;> simp_all [run, step, sourceEvent, observe, snapshotEvent, Served.read,
    selectedRootSnapshot]

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
  cases status <;> simp_all [run, step, sourceEvent, observe, snapshotEvent, Served.read,
    selectedRootSnapshot]

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
  cases status <;> simp_all [run, step, sourceEvent, observe, snapshotEvent, Served.read,
    selectedRootSnapshot]

/-- Ready means the selected Node is visible; an unavailable Node is blocked,
and a transport outage is waiting rather than an access refusal. -/
theorem settled_presentation_names_the_cause (granted refuses : Bool) (status : Status)
    (h : status ≠ .noEndpoint) (n : Nat) :
    presentation (some (run ⟨true, true, false⟩ (n + 2) status)) = (.ready, .connected) ∧
    presentation (some (run ⟨false, granted, refuses⟩ (n + 2) status)) =
      (.waiting, .disconnected) ∧
    presentation (some (run ⟨true, false, false⟩ (n + 2) status)) = (.blocked, .refused) := by
  refine ⟨?_, ?_, ?_⟩
  · rw [reachable_settles_connected status h n]; rfl
  · rw [unreachable_settles_disconnected granted refuses status h n]; rfl
  · rw [hidden_node_settles_refused status h n]; rfl

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

/-! Which requests a node's snapshot shows, and when one session scope's share
of them can be read alone. The snapshot selects its roots (every non-terminal
request, every session's head request, each trigger's newest candidates) and
closes them over the request links (the owner of a folded or replaced request,
a retry's parent and children, the request a queued one waits behind). Every
link stays inside one session scope (node, session and requester:
`request_closure.rs` refuses a linked row outside its scope). Candidates are
the one root chosen across scopes. -/
namespace SessionRequestReload

/-- A request as the snapshot's selection sees it. -/
structure Request where
  scope : Nat
  /-- Non-terminal, or its session's head request: a root read from its scope. -/
  seed : Bool
  /-- Among its trigger's newest candidates: a root read per trigger. -/
  candidate : Bool
  /-- A trigger caused it. -/
  triggered : Bool

/-- `b` is reached from `a` over the request links. -/
inductive Reaches (linked : Request → Request → Prop) : Request → Request → Prop where
  | root (r : Request) : Reaches linked r r
  | link {a b c : Request} : Reaches linked a b → linked b c → Reaches linked a c

/-- The requests a full load of the node shows. -/
def Shown (linked : Request → Request → Prop) (r : Request) : Prop :=
  ∃ root, (root.seed = true ∨ root.candidate = true) ∧ Reaches linked root r

/-- The requests a read of one session scope shows: that scope's non-terminal
requests and its head, closed over the links. -/
def ShownFromScope (linked : Request → Request → Prop) (scope : Nat) (r : Request) : Prop :=
  ∃ root, root.seed = true ∧ root.scope = scope ∧ Reaches linked root r

theorem reaches_keeps_scope {linked : Request → Request → Prop}
    (hscope : ∀ a b, linked a b → a.scope = b.scope) {a b : Request}
    (h : Reaches linked a b) : a.scope = b.scope := by
  induction h with
  | root => rfl
  | link _ hl ih => exact ih.trans (hscope _ _ hl)

/-- A scope holding no trigger candidate is derivable alone: the requests a
full load shows for it are exactly those a read of the scope shows. Replacing
the scope's rows with that read leaves the store as a full load would. -/
theorem session_requests_derive_alone {linked : Request → Request → Prop}
    (hscope : ∀ a b, linked a b → a.scope = b.scope) (scope : Nat)
    (hnone : ∀ r : Request, r.candidate = true → r.scope ≠ scope) (r : Request) :
    (Shown linked r ∧ r.scope = scope) ↔ ShownFromScope linked scope r := by
  constructor
  · rintro ⟨⟨root, hroot, hreach⟩, hr⟩
    have hsame : root.scope = scope := (reaches_keeps_scope hscope hreach).trans hr
    rcases hroot with hseed | hcand
    · exact ⟨root, hseed, hsame, hreach⟩
    · exact absurd hsame (hnone root hcand)
  · rintro ⟨root, hseed, hsame, hreach⟩
    exact ⟨⟨root, Or.inl hseed, hreach⟩, (reaches_keeps_scope hscope hreach).symm.trans hsame⟩

/-- A scope with a candidate is not: the candidate is shown by a full load
and, unless it is also a seed or linked from one, by no read of its scope. -/
theorem candidate_is_shown_by_the_full_load (linked : Request → Request → Prop)
    (r : Request) (h : r.candidate = true) : Shown linked r :=
  ⟨r, Or.inr h, .root r⟩

/-- Whether a session scope's requests may be replaced by a read of that scope
alone: the store holds no trigger-caused request for the scope and the read
returned none. A request becomes a candidate only by a trigger-caused write,
which is a node change (`ObserverResync.Change`), so the full load that
follows it puts the candidate in the store; a scope whose stored and read
requests no trigger caused therefore has no candidate
(`untriggered_scope_has_no_candidate`). The test is wider than candidacy,
which only the per-trigger read knows: a triggered request that is no longer
a candidate also sends its scope to the full load. -/
def derivesAlone (heldTriggered readTriggered : Bool) : Bool :=
  !heldTriggered && !readTriggered

theorem untriggered_scope_has_no_candidate (scope : Nat)
    (hcandidate : ∀ r : Request, r.candidate = true → r.triggered = true)
    (huntriggered : ∀ r : Request, r.scope = scope → r.triggered = false) :
    ∀ r : Request, r.candidate = true → r.scope ≠ scope := by
  intro r hcand hsame
  have := hcandidate r hcand
  simp [huntriggered r hsame] at this

theorem derives_alone_iff_untriggered (heldTriggered readTriggered : Bool) :
    derivesAlone heldTriggered readTriggered = true ↔
      heldTriggered = false ∧ readTriggered = false := by
  cases heldTriggered <;> cases readTriggered <;> simp [derivesAlone]

end SessionRequestReload

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

/-- What a change delivered on a node's subscription is, by its collection. A
`collection` is a configuration collection whose snapshot root is one
self-contained view: its rows depend on no other root and no other root's
follow-up reads depend on them. A `sessionScope` change is a session
document's, or that of a request no trigger caused, whose event names its
session scope and is not a deletion: the session row and the requests the
snapshot shows for that scope are derivable from the scope alone
(`SessionRequestReload.session_requests_derive_alone`; a node holds one
session document per session id, and the snapshot reads it only for its head
request). `node` is every other change: the Node root, whose absence is a
refusal, the triggers, a request a trigger caused, whose candidacy spans
sessions, and a deleted or unscoped session or request. -/
inductive Change where
  | toolControl | transcript | toolCall | collection | sessionScope | node
  deriving DecidableEq, Repr

/-- What the observer does with a delivered change. -/
inductive Delivery where
  /-- `ClientDataPublication.transcriptInvalidation`; consumers re-read the
  bounded transcript of the selected session. -/
  | invalidateTranscript
  /-- `ClientDataPublication.provenanceInvalidation`. -/
  | invalidateProvenance
  /-- A request event, and the collection is queued for its own reload. -/
  | reloadCollection
  /-- A request event, and the change's session scope is queued for the
  node's partial read, which reads that scope's session row and requests
  beside any queued collection. -/
  | reloadSessionScope
  /-- A request event, and the node is queued for a full load. -/
  | resync
  deriving DecidableEq, Repr

/-- A collection change re-reads that collection's root, not the node. A full
load per change re-reads every session, agent and request the node holds:
a runtime whose backend probe rewrites each backend observation's catalog
freshness every minute made every connected client read hundreds of kilobytes
a minute to show a few changed rows. A request is written several times in
every turn (created, claimed, renewed, finished), and its session document
beside it (its head, its activity), so the same holds for them: their session
scope is read again, not the node. -/
def deliver : Change → Delivery
  | .toolControl | .transcript => .invalidateTranscript
  | .toolCall => .invalidateProvenance
  | .collection => .reloadCollection
  | .sessionScope => .reloadSessionScope
  | .node => .resync

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
  /-- Collections (`Delivery.reloadCollection`) or session scopes
  (`Delivery.reloadSessionScope`) are queued for the node's partial read: one
  read of the queued collections' roots and the queued scopes' rows. -/
  collections : Bool := false
  /-- A partial read is in flight. It and the full load never overlap:
  one read per node is in flight. -/
  reloadingCollections : Bool := false
  deriving DecidableEq, Repr

def initial : Slot :=
  { pending := false, waiting := false, loading := false, armed := false, status := .connecting }

/-- `SourceStarted`: a new source for the node, configured, replaced or
reconnected. The previous source's read is aborted. -/
def sourceStarted (n : Slot) : Slot :=
  { n with loading := false, reloadingCollections := false, armed := false,
           status := .connecting }

/-- `SubscriptionsReady`: every root is armed, so the snapshot may start. A
refused slot stays refused until a read is accepted
(`RuntimeConnectionObservation.observe`). -/
def armed (n : Slot) : Slot :=
  { n with pending := true, armed := true,
           status := if n.status = .refused then .refused else .connecting }

/-- A delivered event, a direct refresh the fence rejected, or an explicit
resync request queues the node; queued requests coalesce into one load. -/
def queue (n : Slot) : Slot := { n with pending := true }

/-- A load starts only on an armed source with no other read of the node in
flight. A refused slot is unarmed until its reconnect arms it. -/
def startable (n : Slot) : Bool :=
  n.pending && !n.waiting && !n.loading && !n.reloadingCollections && n.armed

/-- A full load reads every collection after the events that queued them, so
it takes the queued collections with it. -/
def start (n : Slot) : Slot :=
  if startable n then { n with pending := false, loading := true, collections := false } else n

/-- A delivered collection change queues the collection; queued collections
coalesce into one collection reload. -/
def queueCollection (n : Slot) : Slot := { n with collections := true }

/-- A collection reload waits for a queued or running full load, which reads
the collection too, and for the back-off. Full loads go first: a collection
publication would supersede a full load in flight, so collection changes on
every load would keep the node's other rows from ever catching up. -/
def collectionStartable (n : Slot) : Bool :=
  n.collections && !n.pending && !n.waiting && !n.loading && !n.reloadingCollections && n.armed

/-- A collection reload takes every queued collection: one read of their roots. -/
def startCollections (n : Slot) : Slot :=
  if collectionStartable n then { n with collections := false, reloadingCollections := true }
  else n

/-- How a finished collection reload settles. -/
inductive CollectionSettlement where
  | published
  /-- A publication into the node's store since the capture rejected it. -/
  | superseded
  /-- The read failed; the full load owns the status, back-off and refusal. -/
  | failed
  /-- A session scope it read cannot be derived alone
  (`SessionRequestReload.derivesAlone`); nothing is published and the full
  load reads the node. -/
  | deferred
  deriving DecidableEq, Repr

/-- A partial read replaces only its collections' rows and its session scopes'
session row and requests, through the reload fence (`ClientDataPublication.acceptsReload`): a
request event since its capture leaves it publishable, a publication rejects
it. `derivable` is `SessionRequestReload.derivesAlone` of every session scope
it read, decided against the store it publishes into; a read of collections
alone is derivable. The reload fence, not the patch fence, because every
event after the capture queues its own read: a row read before that event is
read again after it, while under `accepts` one request event per read (a lease
renewal, the turn's own writes) would reject every partial read
(`ObserverCollectionReload` TLA, `EventFence`). -/
def collectionSettlement (s : State) (read : Read) (derivable : Bool) :
    LoadResult → CollectionSettlement
  | .rows =>
      if !derivable then .deferred
      else if acceptsReload s read then .published else .superseded
  | .transportFailed | .refused => .failed

/-- A superseded partial read queues its collections and scopes again; a
failed or deferred one queues the node's full load, which reports a failure
(`settle`). None changes the shown status: only a full load shows a node
connected. -/
def settleCollections (n : Slot) : CollectionSettlement → Slot
  | .published => { n with reloadingCollections := false }
  | .superseded => { n with reloadingCollections := false, collections := true }
  | .failed | .deferred => { n with reloadingCollections := false, pending := true }

def timer (n : Slot) : Slot := { n with waiting := false }

/-- A dropped stream aborts the node's read and disarms the source. A queued
load stays queued; it starts once the transport task re-arms the source, and
that load reads every collection whose reload the drop aborted. -/
def streamFailed (n : Slot) : Slot :=
  { n with loading := false, reloadingCollections := false, armed := false,
           status := .disconnected }

def settle (n : Slot) : Settlement → Slot
  | .published => { n with loading := false, status := .connected }
  | .superseded => { n with loading := false, pending := true }
  | .transportFailed =>
      { n with loading := false, pending := true, waiting := true, status := .disconnected }
  | .refused =>
      { n with loading := false, pending := false, waiting := false, armed := false,
               collections := false, reloadingCollections := false, status := .refused }

/-- A refusal the stream reports settles the slot as a refused load does. -/
def streamRefused (n : Slot) : Slot := settle n .refused

/-- Whether a reconnect the user asked for restarts the node's source: only
while the node is not connected. Callers ask without checking the status
themselves (the window shown again asks for every node); a connected source
is the runtime answering, and restarting it would only drop its subscriptions
and its running load. -/
def retryNowRestarts (n : Slot) : Bool := n.status != .connected

/-- The user asked to reconnect the node now
(`RuntimeConnectionObservation.retryNowCount`). A node that is not connected
has its source restarted at the same endpoint as a due reconnect restarts it:
the running load is aborted and the slot is unarmed until the new source arms,
and the shown status is kept (an outage stays `disconnected` until the runtime
answers). The retry timer is cleared, so the re-arm's load starts at once. A
connected node is left as it is (`retryNowRestarts`). -/
def retryNow (n : Slot) : Slot :=
  if retryNowRestarts n then
    { n with waiting := false, loading := false, reloadingCollections := false, armed := false }
  else n

/-- What the desktop's window and network tell the observer. -/
inductive AppEvent where
  /-- The window is shown again after being hidden. -/
  | shownAgain
  | hidden
  /-- The window takes focus without having been hidden. -/
  | focused
  /-- The device comes back online (the browser's `online`). -/
  | online
  /-- The device's network changes, for instance between Wi-Fi and mobile
  data, which stays online (`navigator.connection` `change`). -/
  | networkChanged
  deriving DecidableEq, Repr

/-- Whether an app event asks every node to reconnect now. A node lost to
the network waits out its reconnect backoff, and each of these is evidence
the network it waited on may be back; a hide or a focus alone is not. -/
def asksRetryNow : AppEvent → Bool
  | .shownAgain | .online | .networkChanged => true
  | .hidden | .focused => false

/-- What an app event does to a node: a retry now when the event asks for one. -/
def onAppEvent (event : AppEvent) (n : Slot) : Slot :=
  if asksRetryNow event then retryNow n else n

/-- No app event restarts a connected node: its subscriptions and running
load stay as they are. -/
theorem an_app_event_leaves_a_connected_node (event : AppEvent) (n : Slot)
    (connected : n.status = .connected) : onAppEvent event n = n := by
  unfold onAppEvent retryNow retryNowRestarts
  cases asksRetryNow event <;> simp [connected]

/-- A network change or a return online restarts a node lost to the network
at once, rather than after its backoff (`retryNowCount`). -/
theorem a_network_change_restarts_a_lost_node (n : Slot)
    (lost : n.status = .disconnected) :
    onAppEvent .networkChanged n = retryNow n ∧ onAppEvent .online n = retryNow n ∧
      retryNowRestarts n = true := by
  simp [onAppEvent, asksRetryNow, retryNowRestarts, lost]

/-- A hide or a focus alone asks nothing. -/
theorem a_hide_or_focus_asks_nothing (n : Slot) :
    onAppEvent .hidden n = n ∧ onAppEvent .focused n = n := by
  simp [onAppEvent, asksRetryNow]

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
  | mk pending waiting loading armed status collections reloading =>
    cases status <;> cases pending <;> cases waiting <;> cases loading <;> cases armed <;>
      simp_all [present]

/-- An owned connecting or disconnected slot names its next step: a load can
start now, the retry timer is due, a load or a collection reload is running,
or the source is arming. -/
theorem owned_names_next_step (n : Slot) (h : Owned n)
    (hs : n.status = .connecting ∨ n.status = .disconnected ∨ n.status = .refused) :
    startable n = true ∨ n.waiting = true ∨ n.loading = true ∨
      n.reloadingCollections = true ∨ n.armed = false := by
  obtain ⟨hWaiting, hConnecting, hDisconnected, hRefused⟩ := h
  cases n with
  | mk pending waiting loading armed status collections reloading =>
    rcases hs with hs | hs | hs <;> subst hs <;>
      cases pending <;> cases waiting <;> cases loading <;> cases armed <;> cases reloading <;>
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

theorem retry_now_owned (n : Slot) (h : Owned n) : Owned (retryNow n) := by
  unfold retryNow
  split
  · cases n; simp_all [Owned]
  · exact h

/-- A reconnect the user asked for never waits out a retry timer: the new
source's arm starts its load at once, and the status shown meanwhile is kept. -/
theorem retry_now_loads_on_arm (n : Slot) (h : n.status ≠ .connected) :
    startable (armed (retryNow n)) = true ∧ (retryNow n).status = n.status := by
  simp [retryNow, retryNowRestarts, h, startable, armed]

/-- Asking to reconnect a connected node changes nothing: its source, its
running load and its retry state stay as they are. -/
theorem retry_now_leaves_connected (n : Slot) (h : n.status = .connected) :
    retryNow n = n := by
  simp [retryNow, retryNowRestarts, h]

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

theorem start_of_not_startable (n : Slot) (h : startable n = false) : start n = n := by
  simp [start, h]

theorem one_load_per_start (n : Slot) :
    start (start n) = start n := by
  by_cases h : startable n = true
  · exact start_of_not_startable _ (by simp only [start, h, ↓reduceIte]; simp [startable])
  · rw [start_of_not_startable n (by simpa using h)]
    exact start_of_not_startable n (by simpa using h)

theorem queue_collection_owned (n : Slot) (h : Owned n) : Owned (queueCollection n) := by
  cases n; simp_all [Owned, queueCollection]

theorem start_collections_owned (n : Slot) (h : Owned n) : Owned (startCollections n) := by
  unfold startCollections
  split
  · cases n; simp_all [Owned]
  · exact h

theorem settle_collections_owned (n : Slot) (h : Owned n) (outcome : CollectionSettlement) :
    Owned (settleCollections n outcome) := by
  cases n; cases outcome <;> simp_all [Owned, settleCollections]

/-- One read per node: a full load and a collection reload never start beside
each other. -/
theorem one_read_in_flight (n : Slot) :
    (startable n = true → n.reloadingCollections = false) ∧
      (collectionStartable n = true → n.loading = false) ∧
      startable (startCollections n) = (startable n && !collectionStartable n) ∧
      collectionStartable (start n) = (collectionStartable n && !startable n) := by
  cases n with
  | mk pending waiting loading armed status collections reloading =>
    cases pending <;> cases waiting <;> cases loading <;> cases armed <;> cases collections <;>
      cases reloading <;> simp [startable, collectionStartable, start, startCollections]

/-- A full load takes the queued collections: its read starts after every
event that queued them. -/
theorem full_load_takes_queued_collections (n : Slot) (h : startable n = true) :
    (start n).collections = false := by
  simp [start, h]

/-- A queued full load goes before any queued collection. -/
theorem queued_load_goes_first (n : Slot) (h : n.pending = true) :
    collectionStartable n = false := by
  simp [collectionStartable, h]

/-- A collection reload starts once nothing else is queued or running for an
armed node. -/
theorem queued_collection_starts (n : Slot) (hc : n.collections = true) (hp : n.pending = false)
    (hw : n.waiting = false) (hl : n.loading = false) (hr : n.reloadingCollections = false)
    (ha : n.armed = true) : (startCollections n).reloadingCollections = true := by
  simp [startCollections, collectionStartable, hc, hp, hw, hl, hr, ha]

/-- However many request events arrive while a collection reload runs, it
publishes (the safety half of the TLA model's EventualPublication). -/
theorem request_events_never_supersede_collections (s : State) (events : Nat) :
    collectionSettlement (requestEvents s events) (beginRead s) true .rows = .published := by
  have h := arbitrarily_many_request_events_preserve_reload s events
  simp [collectionSettlement, h]

/-- NoStaleCommit for collection reloads: a publication since capture
supersedes the reload, and its collections are queued for a fresh read. -/
theorem publication_supersedes_collections_and_requeues (s : State) (n : Slot) :
    collectionSettlement (dataChange s) (beginRead s) true .rows = .superseded ∧
      (settleCollections n
        (collectionSettlement (dataChange s) (beginRead s) true .rows)).collections = true := by
  have h := publication_rejects_reload s
  simp [collectionSettlement, h, settleCollections]

/-- A failed collection read is not lost: the node's full load is queued. -/
theorem failed_collections_queue_full_load (n : Slot) (result : LoadResult)
    (h : result ≠ .rows) (s : State) (read : Read) (derivable : Bool) :
    (settleCollections n (collectionSettlement s read derivable result)).pending = true := by
  cases result <;> simp_all [collectionSettlement, settleCollections]

/-- A partial read holding a scope that cannot be derived alone publishes
nothing, whatever it read and whatever the fence says, and the node's full
load is queued. -/
theorem underivable_scope_queues_full_load (n : Slot) (s : State) (read : Read)
    (result : LoadResult) :
    collectionSettlement s read false result ≠ .published ∧
      (settleCollections n (collectionSettlement s read false result)).pending = true := by
  cases result <;> simp [collectionSettlement, settleCollections]

/-- A partial read is a publication: one accepted since a reload's capture
rejects that reload, so no reload in flight replaces the rows it published
with the older rows it read. -/
theorem published_partial_read_rejects_reload_in_flight (s : State) (read : Read)
    (h : collectionSettlement s read true .rows = .published) :
    acceptsReload (ClientDataPublication.reload s read) (beginRead s) = false := by
  have ha : acceptsReload s read = true := by
    by_cases ha : acceptsReload s read = true
    · exact ha
    · simp [collectionSettlement, ha] at h
  simp [ClientDataPublication.reload, ha, publication_rejects_reload]

/-- A collection reload never changes the shown status. -/
theorem collection_reload_keeps_status (n : Slot) (outcome : CollectionSettlement) :
    (settleCollections n outcome).status = n.status := by
  cases outcome <;> rfl

/-- Only a collection change reloads a collection; a node change still loads
the whole node. -/
theorem only_collections_reload_alone (change : Change) :
    deliver change = .reloadCollection ↔ change = .collection := by
  cases change <;> simp [deliver]

/-- Only a session scope change reads its session scope alone. -/
theorem only_session_scope_changes_reload_their_scope (change : Change) :
    deliver change = .reloadSessionScope ↔ change = .sessionScope := by
  cases change <;> simp [deliver]

/-- A full load is queued by a node change alone: no collection or session
request change reads the whole node. -/
theorem only_node_changes_resync (change : Change) :
    deliver change = .resync ↔ change = .node := by
  cases change <;> simp [deliver]

end ObserverResync
