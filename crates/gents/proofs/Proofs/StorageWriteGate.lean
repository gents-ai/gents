/-!
# Storage write-gate scheduling obligations

This is an obligation model, not a proof of DefraDB or Tokio. A finalizer may
hold the existing mutation gate while its stream is pending. A sibling branch
which awaits that gate on the same task can suspend the only poller of the
finalizer, including its timeout. Independent scheduling removes that particular
circular wait, but still needs executor service and storage/cleanup return.

`elapsed` is deliberately distinct from observing a deadline. Dropping a future
is not assumed to preempt a non-yielding poll or to make cleanup return. Gate
release proves neither commit nor rollback. There is no new durable lifecycle.
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
  | siblingWait | elapsed | storageReturned | deadlineObserved
  | cleanupReturned | cleanupDeadlineObserved | dropReturned | release
  deriving DecidableEq, Repr

def pollable (s : State) : Bool := s.independent || !s.siblingWaiting

def held (s : State) : Bool := s.phase != .released

/-- Events ending in `Returned` are external observations, not guarantees. -/
def step (s : State) (event : Event) : State :=
  if event == .siblingWait then { s with siblingWaiting := held s }
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

theorem shared_task_wait_suspends_owner (e : Event) : step suspended e = suspended := by
  cases e <;> decide

theorem elapsed_is_not_timeout_observation (s : State) : step s .elapsed = s := by
  cases s with
  | mk phase independent waiting =>
    cases phase <;> cases independent <;> cases waiting <;> rfl

theorem cleanup_keeps_gate : held (step ⟨.storage, true, true⟩ .deadlineObserved) = true := rfl

theorem cleanup_timeout_still_needs_drop_return :
    held (run ⟨.storage, true, true⟩ [.deadlineObserved, .cleanupDeadlineObserved]) = true := rfl

theorem independent_owner_can_be_polled (s : State) (h : s.independent = true) :
    pollable s = true := by simp [pollable, h]

/-- Conditional bounded path, NOT unconditional eventual storage completion. -/
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

def ValidTrace (states : Nat → State) (events : Nat → Event) : Prop :=
  ∀ n, states (n + 1) = step (states n) (events n)

/-- A real trace counterexample: passage of time cannot service an unpolled
    finalizer. No claim of wall-clock liveness follows from this model alone. -/
theorem suspended_forever_is_valid :
    ValidTrace (fun _ => suspended) (fun _ => .elapsed) ∧
    ∀ n : Nat, held ((fun _ : Nat => suspended) n) = true := by
  constructor
  · intro n; exact (shared_task_wait_suspends_owner .elapsed).symm
  · intro n; rfl

end StorageWriteGate
