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

/-- Returning to the same selection does not revive work from the prior visit. -/
theorem navigate_away_and_back_ignores_completion (s : View α) (other result : α) :
    finish (select (select s other) s.value) s.epoch result =
      select (select s other) s.value := by
  have hne : s.epoch + 1 + 1 ≠ s.epoch := by omega
  simp [finish, select, accepts, hne]

theorem older_epoch_cannot_commit (s : View α) (captured : Nat) (result : α)
    (h : captured < s.epoch) : finish s captured result = s := by
  have hne : s.epoch ≠ captured := by omega
  simp [finish, accepts, hne]

/-- Passive observation / remote completion does not create a new intent. -/
theorem completion_preserves_epoch (s : View α) (captured : Nat) (result : α) :
    (finish s captured result).epoch = s.epoch := by
  simp only [finish]
  split <;> rfl

/-- Releasing this operation's pending marker is separate from publishing its
result. A stale result still releases its own marker, but never another one's. -/
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
def observeFirstRun (phase : FirstRunPhase) (snapshotReady initConfigAbsent needsSetup authoritative : Bool) : FirstRunPhase :=
  match phase with
  | .active => .active
  | .done => .done
  | .unknown =>
      if !snapshotReady then .unknown
      else if initConfigAbsent then .active
      else if !authoritative then .unknown
      else if needsSetup then .active else .done

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

/-! A data-read publication fence is separate from the shell's UI-intent epoch
and lineage provenance. The native projection owner compares retained snapshot
identity and a request-event generation. Transcript-only invalidations do not
replace the snapshot or alter either read-fence component; an observed request
event advances its own generation even when no row is cached. -/
namespace ClientDataPublication

structure State where
  dataEpoch : Nat
  provenanceEpoch : Nat
  requestEventEpoch : Nat
  transcriptEpoch : Nat
  deriving DecidableEq, Repr

structure Read where
  dataEpoch : Nat
  requestEventEpoch : Nat
  deriving DecidableEq, Repr

def beginRead (s : State) : Read :=
  { dataEpoch := s.dataEpoch, requestEventEpoch := s.requestEventEpoch }

def accepts (s : State) (read : Read) : Bool :=
  decide (s.dataEpoch = read.dataEpoch ∧ s.requestEventEpoch = read.requestEventEpoch)

def publish (s : State) (read : Read) : State :=
  if accepts s read then { s with dataEpoch := s.dataEpoch + 1 } else s

def requestEvent (s : State) : State :=
  { s with requestEventEpoch := s.requestEventEpoch + 1 }

def transcriptInvalidation (s : State) : State :=
  { s with transcriptEpoch := s.transcriptEpoch + 1 }

theorem newer_data_rejects_old_read (s : State) (read : Read)
    (h : read.dataEpoch < s.dataEpoch) : publish s read = s := by
  have hne : s.dataEpoch ≠ read.dataEpoch := by omega
  simp [publish, accepts, hne]

theorem uncached_request_event_rejects_old_read (s : State) :
    publish (requestEvent s) (beginRead s) = requestEvent s := by
  simp [publish, requestEvent, beginRead, accepts]

theorem request_event_does_not_invalidate_lineage_cache (s : State) :
    (requestEvent s).provenanceEpoch = s.provenanceEpoch := by
  rfl

theorem transcript_invalidation_preserves_read (s : State) :
    accepts (transcriptInvalidation s) (beginRead s) = true := by
  simp [transcriptInvalidation, beginRead, accepts]

theorem accepted_publication_advances_data_epoch (s : State) :
    (publish s (beginRead s)).dataEpoch = s.dataEpoch + 1 := by
  simp [publish, beginRead, accepts]

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
