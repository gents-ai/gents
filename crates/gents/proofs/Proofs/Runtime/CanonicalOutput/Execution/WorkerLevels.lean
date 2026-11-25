import Proofs.Runtime.CanonicalOutput.Execution.WorkerCapacity
import Proofs.Automation.Triggers.Queue

/-!
An Agent's request workers: one `WorkerCapacity` pool per causal level. A
worker is held for a whole request, its tool calls and waits included, while a
model call also takes its backend's admission permit for as long as its stream
runs. The workers are sized here and never from a backend's `max_concurrent`,
so the backend's limit bounds what its server runs at once however many
requests are live.
-/
namespace CanonicalOutput.Execution.WorkerLevels

open WorkerCapacity

/-- Decision: a request's level is its causal hop (`CausalHop.nextHop`), and
hops at or past the last level share it. A turn, the delegate it starts and
that delegate's own delegate each take a worker at a level of their own, so
none of them waits for a worker held at the level of the request that caused
it. Runs a turn fires and the wakes returning results to its session keep hop
zero and share the first level with other sessions. -/
def levels : Nat := 3

/-- Decision: a level runs as many requests as one request may keep in the
background (`MAX_BACKGROUNDED_TOOLS_PER_PARENT`), so the whole fan-out of one
turn starts at once on the next level. -/
def levelWorkers : Nat := 8

def level (hop : Nat) : Nat := min hop (levels - 1)

/-- The most requests one Agent runs at once. -/
def workers : Nat := levels * levelWorkers

inductive Lane where
  | auxiliary
  | level (index : Nat)
  deriving DecidableEq, Repr

/-- Decision: a session-title audit or an access review (an auxiliary request)
takes no worker; it has a lane of its own whose worker only starts it. It is
the one request the session queue offers while its session has a request
running (`SessionQueueState.active`), so on a worker level it would wait for
that request, its own session's turn, to end. -/
def lane (auxiliary : Bool) (hop : Nat) : Lane :=
  if auxiliary then .auxiliary else .level (level hop)

theorem auxiliary_requests_wait_for_no_worker (hop : Nat) : lane true hop = .auxiliary := rfl

abbrev Pools := List State

def initial : Pools := List.replicate levels (WorkerCapacity.initial levelWorkers)

def admit (pools : Pools) (hop : Nat) (ticket : Ticket) : Option Pools :=
  (pools[level hop]?.bind (acquire · ticket)).map (pools.set (level hop))

def release (pools : Pools) (hop : Nat) (ticket : Ticket) : Pools :=
  match pools[level hop]? with
  | some pool => pools.set (level hop) (WorkerCapacity.release pool ticket)
  | none => pools

def live (pools : Pools) : Nat := (pools.map fun pool => pool.active.card).sum

def Safe (pools : Pools) : Prop :=
  pools.length = levels ∧
    ∀ pool ∈ pools, WorkerCapacity.Safe pool ∧ pool.activeLimit = levelWorkers

theorem level_lt_levels (hop : Nat) : level hop < levels := by
  unfold level levels; omega

theorem cause_and_caused_levels_differ (hop : Nat) (h : hop + 1 < levels) :
    level (hop + 1) ≠ level hop := by
  unfold level at *; omega

/-- A request is admitted exactly when its own level has a free worker,
whatever runs at every other level. -/
theorem admit_reads_only_its_level (p q : Pools) (hop : Nat) (ticket : Ticket)
    (h : p[level hop]? = q[level hop]?) :
    (admit p hop ticket).isSome = (admit q hop ticket).isSome := by
  unfold admit
  rw [h]
  cases (q[level hop]?.bind fun pool => acquire pool ticket) <;> rfl

/-- A full level never refuses the requests its own requests cause: changing
the cause's level leaves the caused request's admission as it was. -/
theorem caused_admission_ignores_the_cause_level (p : Pools) (pool : State) (hop : Nat)
    (h : hop + 1 < levels) (ticket : Ticket) :
    (admit (p.set (level hop) pool) (hop + 1) ticket).isSome =
      (admit p (hop + 1) ticket).isSome := by
  apply admit_reads_only_its_level
  rw [List.getElem?_set]
  simp [cause_and_caused_levels_differ hop h |>.symm]

theorem initial_safe : Safe initial := by
  refine ⟨by simp [initial], ?_⟩
  intro pool hpool
  simp only [initial, List.mem_replicate] at hpool
  rw [hpool.2]
  exact ⟨WorkerCapacity.initial_safe _, rfl⟩

private theorem safe_set (pools : Pools) (index : Nat) (pool : State) (hs : Safe pools)
    (hpool : WorkerCapacity.Safe pool ∧ pool.activeLimit = levelWorkers) :
    Safe (pools.set index pool) := by
  refine ⟨by simp [hs.1], ?_⟩
  intro member hmember
  rcases List.mem_or_eq_of_mem_set hmember with hold | hnew
  · exact hs.2 member hold
  · rw [hnew]; exact hpool

theorem admit_safe {p q : Pools} {hop : Nat} {ticket : Ticket} (hs : Safe p)
    (h : admit p hop ticket = some q) : Safe q := by
  unfold admit at h
  cases hget : p[level hop]? with
  | none => simp [hget] at h
  | some pool =>
    cases hacquire : acquire pool ticket with
    | none => simp [hget, hacquire] at h
    | some next =>
      simp [hget, hacquire] at h
      subst h
      have hmem : pool ∈ p := List.mem_of_getElem? hget
      have hpool := hs.2 pool hmem
      refine safe_set p (level hop) next hs ⟨acquire_safe hpool.1 hacquire, ?_⟩
      unfold acquire at hacquire
      split at hacquire
      · cases hacquire; exact hpool.2
      · contradiction

theorem release_safe {p : Pools} (hs : Safe p) (hop : Nat) (ticket : Ticket) :
    Safe (release p hop ticket) := by
  unfold release
  cases hget : p[level hop]? with
  | none => exact hs
  | some pool =>
    have hpool := hs.2 pool (List.mem_of_getElem? hget)
    exact safe_set p (level hop) _ hs ⟨WorkerCapacity.release_safe hpool.1 ticket, hpool.2⟩

private theorem sum_bounded (pools : Pools)
    (h : ∀ pool ∈ pools, pool.active.card ≤ levelWorkers) :
    (pools.map fun pool => pool.active.card).sum ≤ pools.length * levelWorkers := by
  induction pools with
  | nil => simp
  | cons head tail ih =>
    simp only [List.map_cons, List.sum_cons, List.length_cons]
    have hhead := h head (List.mem_cons_self _ _)
    have htail := ih fun pool hpool => h pool (List.mem_cons_of_mem _ hpool)
    rw [Nat.succ_mul]
    omega

/-- An Agent never runs more than `workers` requests at once. -/
theorem live_bounded {p : Pools} (hs : Safe p) : live p ≤ workers := by
  unfold live workers
  rw [← hs.1]
  apply sum_bounded
  intro pool hpool
  have hsafe := hs.2 pool hpool
  have := hsafe.1
  unfold WorkerCapacity.Safe at this
  omega

/-- Decision: a worker admits a request to run; whether it runs its session's
turn is the claim's (`Triggers.Durable.observedClaimAllowed`), taken in the
claim transaction, which writes the session's document so two claims of one
session commit one at a time. However many workers hold requests of one
session, a request of a session another request is running is refused and
returns to pending, so a session runs one request at a time, in arrival order;
parallelism is across sessions. -/
theorem a_session_runs_one_request_whatever_the_workers
    (candidate running : Triggers.Durable.ClaimObservation)
    (rows : List Triggers.Durable.ClaimObservation) (hm : running ∈ rows)
    (hd : running.document ≠ candidate.document)
    (howner : candidate.owner = running.owner) (hsession : candidate.session = running.session)
    (hr : running.running = true) (ht : running.terminal = false) :
    Triggers.Durable.observedClaimAllowed candidate rows = false :=
  Triggers.Durable.running_conflict_prevents_claim candidate running rows hm hd
    (by simp [Triggers.Durable.claimConflict, howner, hsession]) hr ht

end CanonicalOutput.Execution.WorkerLevels
