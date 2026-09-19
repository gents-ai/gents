import Proofs.CanonicalOutput.Execution.Transition
import Proofs.StorageWriteGate

/-!
# Local gate / canonical transaction composition

The existing scheduling model describes when the gate holder can be polled and
released. This composition permits only that holder to apply a canonical
operation, against the current durable world rather than a pre-acquisition
snapshot. Operations are pure atomic commit results: native mutex exclusion,
DefraDB commit/rollback and authenticated actor binding remain refinement
obligations. No claim of cross-process exclusion or scheduler fairness is made.
-/
namespace CanonicalOutput.Execution.Gate

abbrev Actor := Nat

inductive Operation where
  | append (generation : Generation) (record : Segment)
  | retract (generation : Generation) (record : Segment)
  | accept (generation : Generation) (closing : Segment)
      (message : MessageEnvelope) (targets : List RemoteTarget)
  | authored (generation : Generation) (closing : Segment) (message : MessageEnvelope)
  | headerOnly (generation : Generation) (message : MessageEnvelope)
  | dispatch (generation : Generation) (permit : DispatchPermit)
  | recover (expected fresh : Generation) (duration deadline : Time) (items : List RecoveryItem)
  | terminalize (generation : Generation) (outcome : RequestExecutionLease.Outcome)
      (selection : TerminalSelection)
  deriving DecidableEq

def evaluate (operation : Operation) (world : World) : Except Error World :=
  match operation with
  | .append generation record => appendRaw world generation record
  | .retract generation record => retractBeforeRetry world generation record
  | .accept generation closing message targets =>
      acceptAndPublish world generation closing message targets
  | .authored generation closing message => publishAuthored world generation closing message
  | .headerOnly generation message => publishHeaderOnly world generation message
  | .dispatch generation permit => Execution.dispatch world generation permit
  | .recover expected fresh duration deadline items =>
      recoverExpiredBatch world expected fresh duration deadline items
  | .terminalize generation outcome selection =>
      Execution.terminalize world generation outcome selection

structure State where
  execution : World
  owner : Option Actor
  schedule : StorageWriteGate.State
  deriving DecidableEq

def initial (world : World) : State := ⟨world, none, ⟨.released, true, false⟩⟩

def acquire (state : State) (actor : Actor) (independent : Bool) : Option State :=
  if state.owner.isSome || StorageWriteGate.held state.schedule then none
  else some { state with owner := some actor, schedule := ⟨.storage, independent, false⟩ }

def scheduling (state : State) (actor : Actor) (event : StorageWriteGate.Event) : Option State :=
  if state.owner != some actor then none
  else
    let schedule := StorageWriteGate.step state.schedule event
    some { state with
      schedule := schedule
      owner := if StorageWriteGate.held schedule then state.owner else none }

def atTime (world : World) (now : Time) : World :=
  { world with lease := { world.lease with now := now } }

/-- The authoritative read and the result are inside the same held gate.
Failed evaluation commits nothing. The holder must still finish cleanup/release
through the scheduling owner; an error is not an implicit unlocked state. -/
def commit (state : State) (actor : Actor) (now : Time) (operation : Operation) : Option State :=
  if state.owner != some actor || state.schedule.phase != .storage ||
      !StorageWriteGate.pollable state.schedule || now < state.execution.lease.now then none
  else match evaluate operation (atTime state.execution now) with
    | .error _ => none
    | .ok execution => some { state with
        execution := execution
        schedule := { state.schedule with phase := .releasable } }

theorem other_actor_cannot_commit (state : State) (actor : Actor) (now : Time)
    (operation : Operation) (h : state.owner ≠ some actor) :
    commit state actor now operation = none := by
  simp [commit, h]

theorem successful_commit_identifies_holder (before after : State) (actor : Actor)
    (now : Time) (operation : Operation)
    (h : commit before actor now operation = some after) : before.owner = some actor := by
  by_contra howner
  rw [other_actor_cannot_commit before actor now operation howner] at h
  contradiction

theorem two_successes_from_one_gate_have_same_actor
    (before left right : State) (actorLeft actorRight : Actor) (timeLeft timeRight : Time)
    (opLeft opRight : Operation)
    (hleft : commit before actorLeft timeLeft opLeft = some left)
    (hright : commit before actorRight timeRight opRight = some right) :
    actorLeft = actorRight := by
  have hl := successful_commit_identifies_holder before left actorLeft timeLeft opLeft hleft
  have hr := successful_commit_identifies_holder before right actorRight timeRight opRight hright
  exact Option.some.inj (hl.symm.trans hr)

theorem held_gate_cannot_be_acquired (state : State) (actor : Actor) (independent : Bool)
    (h : StorageWriteGate.held state.schedule = true) :
    acquire state actor independent = none := by
  simp [acquire, h]

theorem suspended_holder_cannot_commit (state : State) (actor : Actor) (now : Time)
    (operation : Operation) (h : state.schedule = StorageWriteGate.suspended) :
    commit state actor now operation = none := by
  simp [commit, h, StorageWriteGate.suspended, StorageWriteGate.pollable]

theorem scheduling_preserves_durable_world (before after : State) (actor : Actor)
    (event : StorageWriteGate.Event) (h : scheduling before actor event = some after) :
    after.execution = before.execution := by
  unfold scheduling at h
  split at h
  · contradiction
  · cases h; rfl

theorem acquire_preserves_durable_world (before after : State) (actor : Actor)
    (independent : Bool) (h : acquire before actor independent = some after) :
    after.execution = before.execution := by
  unfold acquire at h
  split at h
  · contradiction
  · cases h; rfl

/-- A core equation, not a postcondition rechecked by a wrapper: successful
commit evaluated this operation on the latest world held by this gate. -/
theorem commit_reads_current_world (before after : State) (actor : Actor) (now : Time)
    (operation : Operation) (h : commit before actor now operation = some after) :
    evaluate operation (atTime before.execution now) = .ok after.execution := by
  unfold commit at h
  split at h
  · contradiction
  · cases heval : evaluate operation (atTime before.execution now) with
    | error error => simp [heval] at h
    | ok execution => simp [heval] at h; cases h; rfl

theorem committed_gate_stays_held (before after : State) (actor : Actor) (now : Time)
    (operation : Operation) (h : commit before actor now operation = some after) :
    after.owner = before.owner ∧ StorageWriteGate.held after.schedule = true := by
  unfold commit at h
  split at h
  · contradiction
  · cases heval : evaluate operation (atTime before.execution now) with
    | error error => simp [heval] at h
    | ok execution => simp [heval] at h; cases h; exact ⟨rfl, rfl⟩

/-- Even when storage has returned, a sibling cannot acquire until the
existing owner performs the explicit release step. -/
theorem sibling_waits_for_release (before after : State) (actor other : Actor)
    (now : Time) (operation : Operation)
    (h : commit before actor now operation = some after) (independent : Bool) :
    acquire after other independent = none :=
  held_gate_cannot_be_acquired after other independent
    (committed_gate_stays_held before after actor now operation h).2

theorem next_holder_reads_previous_commit
    (committed released acquired after : State) (previous next : Actor)
    (independent : Bool) (now : Time) (operation : Operation)
    (hrelease : scheduling committed previous .release = some released)
    (hacquire : acquire released next independent = some acquired)
    (hcommit : commit acquired next now operation = some after) :
    evaluate operation (atTime committed.execution now) = .ok after.execution := by
  have hread := commit_reads_current_world acquired after next now operation hcommit
  have hreleaseWorld := scheduling_preserves_durable_world committed released previous .release hrelease
  have hacquireWorld := acquire_preserves_durable_world released acquired next independent hacquire
  rwa [hacquireWorld, hreleaseWorld] at hread

end CanonicalOutput.Execution.Gate
