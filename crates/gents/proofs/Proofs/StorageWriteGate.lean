/-!
# Storage write-gate scheduling obligations

This is an obligation model, not a proof of DefraDB or Tokio: dropping a future
is not assumed to preempt a non-yielding poll, and gate release proves neither
commit nor rollback.
-/
namespace StorageWriteGate

inductive Phase where
  | storage | cleanup | dropping | releasable | released
  deriving DecidableEq, Repr

structure State where
  phase : Phase
  independent : Bool
  siblingWaiting : Bool
  deriving DecidableEq, Repr

inductive Event where
  | siblingWait | elapsed | rollbackOwnerScheduled | storageReturned | deadlineObserved
  | cleanupReturned | cleanupDeadlineObserved | dropReturned | release
  deriving DecidableEq, Repr

def pollable (s : State) : Bool := s.independent || !s.siblingWaiting

def held (s : State) : Bool := s.phase != .released

/-- Events ending in `Returned` are external observations, not guarantees;
    `rollbackOwnerScheduled` is not a cancellation request or intent. An armed
    transaction may also be dropped during cleanup after an explicit rollback
    failure, transferring the gate to the detached owner. -/
def step (s : State) (event : Event) : State :=
  if event == .siblingWait then { s with siblingWaiting := held s }
  else if event == .rollbackOwnerScheduled then
    match s.phase with
    | .storage | .cleanup => { s with phase := .cleanup, independent := true }
    | _ => s
  else if !pollable s then s
  else match s.phase, event with
    | .storage, .storageReturned => { s with phase := .releasable }
    | .storage, .deadlineObserved => { s with phase := .cleanup }
    | .cleanup, .cleanupReturned => { s with phase := .releasable }
    | .cleanup, .cleanupDeadlineObserved => { s with phase := .dropping }
    | .dropping, .dropReturned => { s with phase := .releasable }
    | .releasable, .release => { s with phase := .released }
    | _, _ => s


def run (s : State) (events : List Event) : State := events.foldl step s

def suspended : State := ⟨.storage, false, true⟩

theorem released_gate_stays_released (e : Event) :
    (step ⟨.released, false, false⟩ e).phase = .released := by
  cases e <;> rfl

theorem cleanup_handoff_becomes_pollable (waiting : Bool) :
    pollable (step ⟨.cleanup, false, waiting⟩ .rollbackOwnerScheduled) = true := by
  cases waiting <;> rfl

theorem shared_task_wait_suspends_owner (e : Event) (h : e ≠ .rollbackOwnerScheduled) :
    step suspended e = suspended := by
  cases e <;> simp_all [step, suspended, pollable, held]

theorem elapsed_is_not_timeout_observation (s : State) : step s .elapsed = s := by
  cases s with
  | mk phase independent waiting =>
    cases phase <;> cases independent <;> cases waiting <;> rfl

theorem cleanup_keeps_gate : held (step ⟨.storage, true, true⟩ .deadlineObserved) = true := rfl

theorem cleanup_timeout_still_needs_drop_return :
    held (run ⟨.storage, true, true⟩ [.deadlineObserved, .cleanupDeadlineObserved]) = true := rfl

theorem independent_owner_can_be_polled (s : State) (h : s.independent = true) :
    pollable s = true := by simp [pollable, h]

theorem observed_completion_releases (waiting : Bool) :
    held (run ⟨.storage, true, waiting⟩ [.storageReturned, .release]) = false := by
  cases waiting <;> rfl

theorem observed_cleanup_return_releases (waiting : Bool) :
    held (run ⟨.storage, true, waiting⟩
      [.deadlineObserved, .cleanupReturned, .release]) = false := by
  cases waiting <;> rfl

theorem observed_drop_return_releases (waiting : Bool) :
    held (run ⟨.storage, true, waiting⟩
      [.deadlineObserved, .cleanupDeadlineObserved, .dropReturned, .release]) = false := by
  cases waiting <;> rfl

/-- Once `Drop` schedules a detached rollback owner, that owner can be polled
    independently of the cancelled caller; this does not assert that native
    DefraDB work has completed or been rolled back. -/
theorem scheduled_rollback_timeout_releases_gate :
    held (run ⟨.storage, false, true⟩
      [.rollbackOwnerScheduled, .cleanupDeadlineObserved, .dropReturned, .release]) = false := by
  rfl

def ValidTrace (states : Nat → State) (events : Nat → Event) : Prop :=
  ∀ n, states (n + 1) = step (states n) (events n)

theorem suspended_forever_is_valid :
    ValidTrace (fun _ => suspended) (fun _ => .elapsed) ∧
    ∀ n : Nat, held ((fun _ : Nat => suspended) n) = true := by
  constructor
  · intro n; exact (shared_task_wait_suspends_owner .elapsed (by decide)).symm
  · intro n; rfl

/-! The per-operation lifecycle above models one acquired native write permit.
`Admission` models the shared process-local limiter around those independent
operations. A permit remains represented by its own lifecycle state until that
owner observes native completion or cleanup/drop completion and releases it.
The store claim belongs to the shared owner and outlives individual permits.
-/
namespace Admission

/-- The native admission bound limits how many local writes may be active.
    It does not assert that admitted transactions are conflict-free or commit in
    admission order; request/lease validation remains a native OCC refinement. -/
def capacity : Nat := 64

/-- The admission queue bounds concurrent native operations. Each active entry
    tracks one operation's completion/cleanup lifetime; the shared store claim
    may outlive all active permits. -/
structure Pool where
  active : List (Nat × State)
  waiting : List Nat
  storeClaimHeld : Bool
  deriving DecidableEq, Repr

def empty : Pool := ⟨[], [], false⟩

/-- A claimed local store keeps its process-wide claim independently of permits. -/
def claimed : Pool := { empty with storeClaimHeld := true }

def owns (pool : Pool) (owner : Nat) : Bool :=
  (pool.active.any fun entry => entry.1 == owner) || owner ∈ pool.waiting

/-- Enqueue a unique lease identity. Requests keep their identity while waiting
    for a slot, so cancellation can remove only that request. -/
def request (pool : Pool) (owner : Nat) : Pool :=
  if owns pool owner then pool else { pool with waiting := pool.waiting ++ [owner] }

/-- Admit the oldest waiter when capacity is available. Each admitted operation
    receives an independent storage/cleanup lifecycle. -/
def admitNext? (pool : Pool) : Option Pool :=
  if pool.active.length < capacity then
    match pool.waiting with
    | [] => none
    | owner :: rest =>
        some { pool with
          active := pool.active ++ [(owner, ⟨.storage, true, false⟩)]
          waiting := rest }
  else none

def ownerState? (pool : Pool) (owner : Nat) : Option State :=
  (pool.active.find? fun entry => entry.1 = owner).map Prod.snd

/-- Apply a native completion, timeout, cleanup, or drop observation to exactly
    one lease. Other active permits retain their own lifecycle unchanged. -/
def observe (pool : Pool) (owner : Nat) (event : Event) : Pool :=
  { pool with active := pool.active.map fun entry =>
      if entry.1 = owner then (entry.1, step entry.2 event) else entry }

/-- A lease releases its permit only after its own lifecycle reaches released. -/
def release (pool : Pool) (owner : Nat) : Pool :=
  match ownerState? pool owner with
  | some leaseState =>
      if leaseState.phase == .released then
        { pool with active := pool.active.filter fun entry => entry.1 != owner }
      else pool
  | none => pool

def activeCount (pool : Pool) : Nat := pool.active.length

def requestAll (pool : Pool) : List Nat → Pool
  | [] => pool
  | owner :: rest => requestAll (request pool owner) rest

def admitCount (pool : Pool) : Nat → Pool
  | 0 => pool
  | count + 1 => admitCount ((admitNext? pool).getD pool) count

theorem admission_never_exceeds_capacity (pool : Pool) (next : Pool)
    (admitted : admitNext? pool = some next) :
    activeCount next ≤ capacity := by
  unfold admitNext? at admitted
  split at admitted
  · split at admitted
    · simp_all [activeCount]
    · rename_i owner rest
      simp only [Option.some.injEq] at admitted
      subst next
      simp only [activeCount]
      simp_all [capacity]
      omega
  · contradiction

theorem admission_preserves_store_claim (pool : Pool) (next : Pool)
    (admitted : admitNext? pool = some next) :
    next.storeClaimHeld = pool.storeClaimHeld := by
  by_cases hcapacity : pool.active.length < capacity
  · cases hwaiting : pool.waiting with
    | nil => simp [admitNext?, hcapacity, hwaiting] at admitted
    | cons owner rest =>
        simp [admitNext?, hcapacity, hwaiting] at admitted
        cases admitted
        rfl
  · simp [admitNext?, hcapacity] at admitted

theorem request_preserves_store_claim (pool : Pool) (owner : Nat) :
    (request pool owner).storeClaimHeld = pool.storeClaimHeld := by
  by_cases h : owns pool owner <;> simp [request, h]

theorem observe_preserves_store_claim (pool : Pool) (owner : Nat) (event : Event) :
    (observe pool owner event).storeClaimHeld = pool.storeClaimHeld := by
  rfl

theorem release_preserves_store_claim (pool : Pool) (owner : Nat) :
    (release pool owner).storeClaimHeld = pool.storeClaimHeld := by
  cases h : ownerState? pool owner <;> simp [release, h] <;>
    split <;> rfl

theorem unfinished_lease_cannot_release_permit (pool : Pool) (owner : Nat)
    (leaseState : State) (found : ownerState? pool owner = some leaseState)
    (unfinished : leaseState.phase ≠ .released) :
    release pool owner = pool := by
  simp [release, found, unfinished]

theorem independent_leases_can_be_admitted :
    let first := request empty 1
    let afterFirst := (admitNext? first).getD first
    let second := request afterFirst 2
    let afterSecond := (admitNext? second).getD second
    activeCount afterSecond = 2 ∧
      (ownerState? afterSecond 1).isSome ∧
      (ownerState? afterSecond 2).isSome := by
  decide

set_option maxRecDepth 10000 in
theorem sixty_fifth_request_waits_at_capacity :
    let queued := requestAll claimed (List.range (capacity + 1))
    let admitted := admitCount queued (capacity + 1)
    activeCount admitted = capacity ∧ admitted.waiting = [capacity] ∧
      admitted.storeClaimHeld = true := by
  decide

theorem one_completion_does_not_change_sibling_lease :
    let first := request empty 1
    let afterFirst := (admitNext? first).getD first
    let second := request afterFirst 2
    let afterSecond := (admitNext? second).getD second
    let completed := observe afterSecond 1 .storageReturned
    activeCount completed = 2 ∧
      ownerState? completed 2 = some ⟨.storage, true, false⟩ := by
  decide

theorem permit_remains_held_through_drop_cleanup :
    let queued := request empty 7
    let admitted := (admitNext? queued).getD queued
    let timedOut := observe admitted 7 .deadlineObserved
    let cleanupTimedOut := observe timedOut 7 .cleanupDeadlineObserved
    let dropReturned := observe cleanupTimedOut 7 .dropReturned
    activeCount timedOut = 1 ∧ activeCount cleanupTimedOut = 1 ∧
      activeCount dropReturned = 1 ∧
      (ownerState? dropReturned 7).map State.phase = some .releasable := by
  decide

theorem shared_store_claim_outlives_released_permit :
    let queued := request claimed 7
    let admitted := (admitNext? queued).getD queued
    let timedOut := observe admitted 7 .deadlineObserved
    let cleanupReturned := observe timedOut 7 .cleanupReturned
    let ownerReleased := observe cleanupReturned 7 .release
    let permitReleased := release ownerReleased 7
    activeCount permitReleased = 0 ∧ permitReleased.storeClaimHeld = true := by
  decide

end Admission

end StorageWriteGate
