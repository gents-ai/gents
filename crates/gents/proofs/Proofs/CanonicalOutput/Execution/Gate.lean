import Proofs.CanonicalOutput.Execution.Sequence
import Proofs.CanonicalOutput.Execution.ToolDelivery
import Proofs.CanonicalOutput.Execution.Compaction
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
  | renew (generation : Generation) (expectedDeadline : Time)
  | append (generation : Generation) (record : Segment)
  | retract (generation : Generation) (record : Segment)
  | accept (generation : Generation) (closing : Segment)
      (message : MessageEnvelope) (targets : List RemoteTarget)
      (admissions : List ToolAdmission)
  | authored (generation : Generation) (closing : Segment) (message : MessageEnvelope)
  | headerOnly (generation : Generation) (message : MessageEnvelope)
      (admissions : List ToolAdmission)
  | dispatch (generation : Generation) (permit : DispatchPermit)
  | admitSpawned (generation : Generation) (admission : SpawnedToolAdmission)
  | toolControl (generation : Generation) (document : DocId)
      (action : ToolExecution.ToolCallContext.Action)
  | toolAppend (document : DocId) (record : Segment)
  | toolClose (document : DocId) (authority : ToolDelivery.CloseAuthority)
      (record : Segment)
  | toolDeliver (document : DocId) (message : MessageEnvelope)
  | toolGoalDeliver (document : DocId) (binding : GoalNotificationBinding)
      (message : MessageEnvelope)
  | backgroundReceipt (parentDocument : DocId) (closing : Segment)
      (message : MessageEnvelope)
  | compact (cursor : Transcript.Sequence)
  | recover (expected fresh : Generation) (duration deadline : Time) (items : List RecoveryItem)
  | revoke (expected fresh : Generation) (outcome : RequestExecutionLease.Outcome)
      (selection : TerminalSelection)
  | terminalize (generation : Generation) (outcome : RequestExecutionLease.Outcome)
      (selection : TerminalSelection)

inductive Error where
  | execution (error : Execution.Error)
  | delivery (error : ToolDelivery.Error)
  | compactionRejected
  deriving DecidableEq, Repr

def evaluate (operation : Operation) (world : World) : Except Error World :=
  match operation with
  | .renew generation expectedDeadline =>
      (Execution.renew world generation expectedDeadline).mapError .execution
  | .append generation record => (appendRaw world generation record).mapError .execution
  | .retract generation record =>
      (retractBeforeRetry world generation record).mapError .execution
  | .accept generation closing message targets admissions =>
      (acceptAndPublish world generation closing message targets admissions).mapError .execution
  | .authored generation closing message =>
      (publishAuthored world generation closing message).mapError .execution
  | .headerOnly generation message admissions =>
      (publishHeaderOnly world generation message admissions).mapError .execution
  | .dispatch generation permit => (Execution.dispatch world generation permit).mapError .execution
  | .admitSpawned generation admission =>
      (admitSpawnedBackground world generation admission).mapError .execution
  | .toolControl generation document action =>
      (changeToolControl world generation document action).mapError .execution
  | .toolAppend document record =>
      (ToolDelivery.appendToolOutput world document record).mapError .delivery
  | .toolClose document authority record =>
      (ToolDelivery.closeToolOutput world document authority record).mapError .delivery
  | .toolDeliver document message =>
      (ToolDelivery.publishToolDelivery world document message).mapError .delivery
  | .toolGoalDeliver document binding message =>
      (ToolDelivery.publishGoalNotification world document binding message).mapError .delivery
  | .backgroundReceipt parentDocument closing message =>
      (ToolDelivery.publishBackgroundReceipt world parentDocument closing message).mapError .delivery
  | .compact cursor =>
      match Compaction.advanceCursor? world cursor with
      | some after => .ok after
      | none => .error .compactionRejected
  | .recover expected fresh duration deadline items =>
      (recoverExpiredBatch world expected fresh duration deadline items).mapError .execution
  | .revoke expected fresh outcome selection =>
      (revokeCorrupt world expected fresh outcome selection).mapError .execution
  | .terminalize generation outcome selection =>
      (Execution.terminalize world generation outcome selection).mapError .execution

private theorem mapError_success {error₁ error₂ value : Type}
    (f : error₁ → error₂) (result : Except error₁ value) (post : value)
    (h : result.mapError f = .ok post) : result = .ok post := by
  cases result with
  | error error => simp [Except.mapError] at h
  | ok value => simpa [Except.mapError] using h

theorem evaluate_nextSequence_monotone (operation : Operation) (before after : World)
    (h : evaluate operation before = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  unfold evaluate at h
  cases operation with
  | renew generation deadline =>
      have h' := mapError_success Error.execution _ _ h
      rw [renew_preserves_nextSeq before after generation deadline h']
  | append generation record =>
      have h' := mapError_success Error.execution _ _ h
      rw [appendRaw_preserves_nextSeq before after generation record h']
  | retract generation record =>
      have h' := mapError_success Error.execution _ _ h
      rw [retractBeforeRetry_preserves_nextSeq before after generation record h']
  | accept generation closing message targets admissions =>
      exact acceptAndPublish_nextSeq_monotone before after generation closing message
        targets admissions (mapError_success Error.execution _ _ h)
  | authored generation closing message =>
      exact publishAuthored_nextSeq_monotone before after generation closing message
        (mapError_success Error.execution _ _ h)
  | headerOnly generation message admissions =>
      exact publishHeaderOnly_nextSeq_monotone before after generation message admissions
        (mapError_success Error.execution _ _ h)
  | dispatch generation permit =>
      rw [dispatch_preserves_nextSeq before after generation permit
        (mapError_success Error.execution _ _ h)]
  | admitSpawned generation admission =>
      rw [admitSpawnedBackground_preserves_nextSeq before after generation admission
        (mapError_success Error.execution _ _ h)]
  | toolControl generation document action =>
      rw [changeToolControl_preserves_nextSeq before after generation document action
        (mapError_success Error.execution _ _ h)]
  | toolAppend document record =>
      rw [ToolDelivery.append_preserves_nextSeq before after document record
        (mapError_success Error.delivery _ _ h)]
  | toolClose document authority record =>
      rw [ToolDelivery.close_preserves_nextSeq before after document authority record
        (mapError_success Error.delivery _ _ h)]
  | toolDeliver document message =>
      exact ToolDelivery.publication_nextSeq_monotone before after document message
        (mapError_success Error.delivery _ _ h)
  | toolGoalDeliver document binding message =>
      exact ToolDelivery.goal_notification_nextSeq_monotone before after document binding message
        (mapError_success Error.delivery _ _ h)
  | backgroundReceipt document closing message =>
      exact ToolDelivery.background_receipt_nextSeq_monotone before after document closing message
        (mapError_success Error.delivery _ _ h)
  | compact cursor =>
      cases hcompact : Compaction.advanceCursor? before cursor with
      | none => simp [hcompact] at h
      | some post =>
          simp [hcompact] at h
          subst after
          rw [(Compaction.advanceCursor_preserves_publications before post cursor hcompact).1]
  | recover expected fresh duration deadline items =>
      exact recoverExpiredBatch_nextSeq_monotone before after expected fresh duration deadline items
        (mapError_success Error.execution _ _ h)
  | revoke expected fresh outcome selection =>
      rw [revokeCorrupt_preserves_nextSeq before after expected fresh outcome selection
        (mapError_success Error.execution _ _ h)]
  | terminalize generation outcome selection =>
      rw [terminalize_preserves_nextSeq before after generation outcome selection
        (mapError_success Error.execution _ _ h)]

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

/-- An integration trace is made only of successful gate commits and explicit
release/reacquisition steps. It cannot postulate a fixture-only state jump. -/
inductive Trace : State → State → Prop where
  | refl (state : State) : Trace state state
  | acquire {before after : State} (actor : Actor) (independent : Bool)
      (h : Gate.acquire before actor independent = some after) : Trace before after
  | scheduling {before after : State} (actor : Actor) (event : StorageWriteGate.Event)
      (h : Gate.scheduling before actor event = some after) : Trace before after
  | commit {before after : State} (actor : Actor) (now : Time) (operation : Operation)
      (h : Gate.commit before actor now operation = some after) : Trace before after
  | trans {first second third : State} : Trace first second → Trace second third →
      Trace first third

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

theorem successful_commit_nextSequence_monotone
    (before after : State) (actor : Actor) (now : Time) (operation : Operation)
    (h : commit before actor now operation = some after) :
    before.execution.transcript.nextSeq ≤ after.execution.transcript.nextSeq := by
  have heval := commit_reads_current_world before after actor now operation h
  exact evaluate_nextSequence_monotone operation (atTime before.execution now)
    after.execution heval

theorem Trace.nextSequence_monotone {before after : State} (trace : Trace before after) :
    before.execution.transcript.nextSeq ≤ after.execution.transcript.nextSeq := by
  induction trace with
  | refl => exact Nat.le_refl _
  | acquire actor independent h =>
      rw [acquire_preserves_durable_world _ _ actor independent h]
  | scheduling actor event h =>
      rw [scheduling_preserves_durable_world _ _ actor event h]
  | commit actor now operation h =>
      exact successful_commit_nextSequence_monotone _ _ actor now operation h
  | trans left right ihLeft ihRight => exact Nat.le_trans ihLeft ihRight

end CanonicalOutput.Execution.Gate
