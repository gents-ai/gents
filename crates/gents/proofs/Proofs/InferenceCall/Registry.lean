import Proofs.InferenceCall.ControllerBookkeeping

/-! One backend's admission capacity pool (#897, #1366).

A backend document rewrite replaces the admitting controller incarnation
immediately; it never waits for earlier incarnations to drain. All
incarnations of a backend share one pool: permits held by calls admitted
under a replaced incarnation, or before an outage, keep counting against the
backend's capacity until they release. A health veto flaps on probe
timeouts, which are most likely while the server is already saturated, so
forgetting those permits at an outage would grant another full capacity on
every flap. Hung calls are bounded separately by the stream idle timeout.

`connection` is the identity a behavior slot's provider client was built
from: endpoint, credentials and provider/wire protocol, but not capacity or
queue depth. A slot's client is fixed when the slot is built, so a caller is
admitted only while its connection is the admitting one. A capacity-only
rewrite keeps queued callers; a connection change rejects callers built for
the old connection, whether they arrive later or are woken from the queue.
No retry owner on that path rebuilds the client, so the rejection is terminal
for the call rather than retried in place (`BackendConnectionChanged`).

Unavailability and removal close admission: queued callers fail with
`BackendGone` and new callers are rejected, while admitted calls keep their
permits. `held` counts real permits, not persisted InferenceCall rows. -/
namespace InferenceCall.Registry

structure Config where
  connection : Nat
  /-- Observed runtime epoch; rollback may reuse it. Attribution only: it is
  never a replacement trigger. -/
  generation : Nat
  capacity : Nat
  queueDepth : Nat
  available : Bool
  deriving DecidableEq, Repr

/-- Cumulative call outcomes, in the vocabulary Rust reports. -/
structure Tally where
  admitted : Nat
  queueFull : Nat
  gone : Nat
  connectionChanged : Nat
  deriving DecidableEq, Repr

/-- `admitting` is the incarnation that admits new calls; `none` means
admission is closed. `held` counts permits held by every call admitted since
the pool first opened. `queue` lists the connections of parked callers in
FIFO order. -/
structure State where
  desired : Option Config
  admitting : Option Config
  held : Nat
  queue : List Nat
  tally : Tally
  deriving DecidableEq, Repr

def availableDesired (desired : Option Config) : Option Config :=
  desired.filter (·.available)

/-- Connection, capacity and queue depth characterize admission resources;
generation is not a replacement trigger. A metadata-only runtime generation
keeps its owner. -/
def sameResources (a b : Config) : Bool :=
  a.connection == b.connection && a.capacity == b.capacity && a.queueDepth == b.queueDepth

def capacity (s : State) : Nat :=
  (s.admitting.map (·.capacity)).getD 0

/-- FIFO hand-off of free permits to parked callers. A caller built for
another connection is woken, rejected, and passes the permit on. -/
def serve (c : Config) (s : State) : List Nat → State
  | [] => { s with queue := [] }
  | w :: rest =>
      if s.held < c.capacity then
        if w == c.connection then
          serve c { s with held := s.held + 1,
                           tally := { s.tally with admitted := s.tally.admitted + 1 } } rest
        else
          serve c { s with tally := { s.tally with
                           connectionChanged := s.tally.connectionChanged + 1 } } rest
      else { s with queue := w :: rest }

def settle (s : State) : State :=
  match s.admitting with
  | none => s
  | some c => serve c s s.queue

def reconcile (s : State) (desired : Option Config) : State :=
  match availableDesired desired with
  | none =>
      { s with desired := desired, admitting := none, queue := [],
               tally := { s.tally with gone := s.tally.gone + s.queue.length } }
  | some next =>
      match s.admitting with
      | some current =>
          if sameResources current next then { s with desired := desired }
          else settle { s with desired := desired, admitting := some next }
      | none => settle { s with desired := desired, admitting := some next }

/-- A caller built for `slot` asks for admission. -/
def acquire (s : State) (slot : Nat) : State :=
  match s.admitting with
  | none => { s with tally := { s.tally with gone := s.tally.gone + 1 } }
  | some c =>
      if slot != c.connection then
        { s with tally := { s.tally with connectionChanged := s.tally.connectionChanged + 1 } }
      else if s.held < c.capacity then
        { s with held := s.held + 1, tally := { s.tally with admitted := s.tally.admitted + 1 } }
      else if s.queue.length < c.queueDepth then { s with queue := s.queue ++ [slot] }
      else { s with tally := { s.tally with queueFull := s.tally.queueFull + 1 } }

/-- An admitted call returns its permit, whichever incarnation admitted it and
whether or not admission has closed since. -/
def release (s : State) : State :=
  settle { s with held := s.held - 1 }

theorem serve_admitting (c : Config) (s : State) (q : List Nat) :
    (serve c s q).admitting = s.admitting := by
  induction q generalizing s with
  | nil => rfl
  | cons w rest ih =>
      simp only [serve]
      split
      · split
        · exact ih _
        · exact ih _
      · rfl

theorem settle_admitting (s : State) : (settle s).admitting = s.admitting := by
  unfold settle
  split
  · rfl
  · exact serve_admitting _ _ _

theorem metadata_only_reconcile_preserves_admitting
    (s : State) (desired : Option Config) (current next : Config)
    (h_admitting : s.admitting = some current)
    (h_available : availableDesired desired = some next)
    (h_same : sameResources current next = true) :
    reconcile s desired = { s with desired := desired } := by
  simp [reconcile, h_admitting, h_available, h_same]

/-- #1366 and #897: an available desired configuration admits through its
own incarnation as soon as it is reconciled, whatever the prior state, unless
it only restates the current resources. -/
theorem available_desired_admits_without_gap (s : State) (desired : Option Config)
    (next : Config) (h_available : availableDesired desired = some next) :
    ∃ c, (reconcile s desired).admitting = some c ∧ sameResources c next = true := by
  cases h : s.admitting with
  | none =>
      refine ⟨next, ?_, by simp [sameResources]⟩
      simp [reconcile, h_available, h, settle_admitting]
  | some current =>
      cases hs : sameResources current next
      · refine ⟨next, ?_, by simp [sameResources]⟩
        simp [reconcile, h_available, h, hs, settle_admitting]
      · exact ⟨current, by simp [reconcile, h_available, h, hs], hs⟩

theorem serve_within_capacity (c : Config) (s : State) (q : List Nat) :
    (serve c s q).held ≤ max s.held c.capacity := by
  induction q generalizing s with
  | nil => simp [serve]
  | cons w rest ih =>
      simp only [serve]
      split
      · rename_i hlt
        split
        · have := ih { s with held := s.held + 1,
                              tally := { s.tally with admitted := s.tally.admitted + 1 } }
          simp at this
          omega
        · exact ih _
      · simp

/-- A woken caller built for another connection never takes a permit. -/
theorem serve_never_admits_stale (c : Config) (s : State) (q : List Nat)
    (h : ∀ w ∈ q, w ≠ c.connection) : (serve c s q).held = s.held := by
  induction q generalizing s with
  | nil => rfl
  | cons w rest ih =>
      have hw : w ≠ c.connection := h w (by simp)
      have hrest : ∀ v ∈ rest, v ≠ c.connection := fun v hv => h v (by simp [hv])
      simp only [serve]
      split
      · simp only [beq_iff_eq, hw, if_false]
        exact ih _ hrest
      · rfl

/-- Callers built for the admitting connection are never rejected as stale. -/
theorem serve_keeps_current_waiters (c : Config) (s : State) (q : List Nat)
    (h : ∀ w ∈ q, w = c.connection) :
    (serve c s q).tally.connectionChanged = s.tally.connectionChanged := by
  induction q generalizing s with
  | nil => rfl
  | cons w rest ih =>
      have hw : w = c.connection := h w (by simp)
      have hrest : ∀ v ∈ rest, v = c.connection := fun v hv => h v (by simp [hv])
      simp only [serve]
      split
      · simp only [hw, beq_self_eq_true, if_true]
        exact ih _ hrest
      · rfl

/-- A capacity-only rewrite keeps every parked caller of the current
connection: none of them is rejected. -/
theorem capacity_only_rewrite_keeps_queue (s : State) (desired : Option Config)
    (current next : Config) (h_admitting : s.admitting = some current)
    (h_available : availableDesired desired = some next)
    (h_connection : next.connection = current.connection)
    (h_queue : ∀ w ∈ s.queue, w = current.connection) :
    (reconcile s desired).tally.connectionChanged = s.tally.connectionChanged := by
  cases hs : sameResources current next
  · simp only [reconcile, h_available, h_admitting, hs, settle]
    exact serve_keeps_current_waiters next _ s.queue (by simpa [h_connection] using h_queue)
  · simp [reconcile, h_available, h_admitting, hs]

/-- New admissions never raise held permits above the current capacity. -/
theorem acquire_admits_within_capacity (s : State) (slot : Nat)
    (h : s.held < (acquire s slot).held) :
    (acquire s slot).held ≤ capacity (acquire s slot) := by
  unfold acquire at h ⊢
  cases hc : s.admitting with
  | none => simp [hc] at h
  | some c =>
      simp only [hc] at h ⊢
      split_ifs at h ⊢
      all_goals simp_all [capacity]
      all_goals omega

theorem over_capacity_blocks_admission (s : State) (slot : Nat)
    (h : capacity s ≤ s.held) : (acquire s slot).held = s.held := by
  unfold acquire
  cases hc : s.admitting with
  | none => rfl
  | some c =>
      simp [capacity, hc] at h
      simp only
      split
      · rfl
      · simp only [Nat.not_lt.mpr h, if_false]
        split <;> rfl

/-- A caller built for a replaced connection is rejected, not admitted. -/
theorem stale_connection_rejected (s : State) (slot : Nat) (c : Config)
    (h_admitting : s.admitting = some c) (h_stale : slot ≠ c.connection) :
    (acquire s slot).held = s.held ∧ (acquire s slot).queue = s.queue ∧
      (acquire s slot).tally.connectionChanged = s.tally.connectionChanged + 1 := by
  simp [acquire, h_admitting, h_stale]

theorem release_within_capacity (s : State) :
    (release s).held ≤ max (s.held - 1) (capacity s) := by
  unfold release settle
  cases hc : s.admitting with
  | none => simp [hc]
  | some c =>
      simp only [hc]
      have := serve_within_capacity c { s with held := s.held - 1 } s.queue
      simp [capacity, hc] at this ⊢
      omega

/-- B1: an outage does not forget held permits; a reopened pool admits only
the capacity they leave free. -/
theorem outage_carries_held (s : State) (down up : Option Config) (next : Config)
    (h_down : availableDesired down = none) (h_up : availableDesired up = some next) :
    (reconcile (reconcile s down) up).held = s.held := by
  simp [reconcile, h_down, h_up, settle, serve]

/-- Deleting or disabling a backend fails every parked caller and rejects new
ones without admitting anything. -/
theorem unavailable_closes_admission (s : State) (desired : Option Config) (slot : Nat)
    (h : availableDesired desired = none) :
    (reconcile s desired).queue = [] ∧
      (reconcile s desired).tally.gone = s.tally.gone + s.queue.length ∧
      (acquire (reconcile s desired) slot).held = s.held := by
  simp [reconcile, h, acquire]

theorem removed_backend_cannot_be_resurrected (s : State) :
    (release (reconcile s none)).admitting = none := by
  simp [reconcile, availableDesired, release, settle]

theorem reconcile_retains_latest_desired (s : State) (desired : Option Config) :
    (reconcile s desired).desired = desired := by
  have hs : ∀ t : State, (settle t).desired = t.desired := by
    intro t
    unfold settle
    split
    · rfl
    · rename_i c _
      suffices ∀ (u : State) q, (serve c u q).desired = u.desired from this _ _
      intro u q
      induction q generalizing u with
      | nil => rfl
      | cons w rest ih =>
          simp only [serve]
          split
          · split
            · exact ih _
            · exact ih _
          · rfl
  unfold reconcile
  split
  · rfl
  · split
    · split
      · rfl
      · exact hs _
    · exact hs _

/-! ## Semaphore ledger refinement

Rust realizes `held ≤ capacity` with one Tokio semaphore per open period.
Tokio permits cannot go negative, so a decrease below the permits in use is
recorded as `owed` and paid by forgetting permits. `transit` counts permits a
Tokio waiter took but the ledger has not registered. Tokio returns such a
permit straight to the semaphore when the waiting future is dropped
(`abandon`), bypassing the ledger, so `available` can become positive while
`owed` is. Registration therefore pays debt before it admits. -/
namespace Ledger

structure State where
  capacity : Nat
  owed : Nat
  available : Nat
  transit : Nat
  held : Nat
  deriving DecidableEq, Repr

/-- Tokens are conserved: every permit is available, in transit, held, or
owed back. -/
def conserved (l : State) : Prop :=
  l.available + l.transit + l.held = l.capacity + l.owed

def take (l : State) : State :=
  if 0 < l.available then { l with available := l.available - 1, transit := l.transit + 1 } else l

/-- A permit taken from the semaphore is either forgotten to pay debt or
admitted. -/
def register (l : State) : State :=
  if 0 < l.transit then
    if 0 < l.owed then { l with transit := l.transit - 1, owed := l.owed - 1 }
    else { l with transit := l.transit - 1, held := l.held + 1 }
  else l

def abandon (l : State) : State :=
  if 0 < l.transit then { l with transit := l.transit - 1, available := l.available + 1 } else l

/-- Returning a permit, including one from a retired semaphore after an
outage: debt first, otherwise back to the current semaphore. -/
def release (l : State) : State :=
  if 0 < l.held then
    if 0 < l.owed then { l with held := l.held - 1, owed := l.owed - 1 }
    else { l with held := l.held - 1, available := l.available + 1 }
  else l

def resize (l : State) (capacity : Nat) : State :=
  if l.capacity ≤ capacity then
    let repaid := min l.owed (capacity - l.capacity)
    { l with capacity := capacity, owed := l.owed - repaid,
             available := l.available + (capacity - l.capacity - repaid) }
  else
    let forgotten := min l.available (l.capacity - capacity)
    { l with capacity := capacity, available := l.available - forgotten,
             owed := l.owed + (l.capacity - capacity - forgotten) }

/-- Reopening after an outage starts a fresh semaphore charged with the
permits still held. -/
def reopen (held capacity : Nat) : State :=
  ⟨capacity, held - min held capacity, capacity - min held capacity, 0, held⟩

/-- A permit taken from a retired semaphore just before it closed is
registered against the current one. -/
def registerRetired (l : State) : State :=
  if 0 < l.available then { l with available := l.available - 1, held := l.held + 1 }
  else { l with owed := l.owed + 1, held := l.held + 1 }

theorem take_conserved (l : State) (h : conserved l) : conserved (take l) := by
  unfold take conserved at *; split <;> simp at * <;> omega

theorem register_conserved (l : State) (h : conserved l) : conserved (register l) := by
  unfold register conserved at *; split
  · split <;> simp <;> omega
  · exact h

theorem abandon_conserved (l : State) (h : conserved l) : conserved (abandon l) := by
  unfold abandon conserved at *; split <;> simp at * <;> omega

theorem release_conserved (l : State) (h : conserved l) : conserved (release l) := by
  unfold release conserved at *; split
  · split <;> simp <;> omega
  · exact h

theorem resize_conserved (l : State) (capacity : Nat) (h : conserved l) :
    conserved (resize l capacity) := by
  unfold resize conserved at *; split <;> simp <;> omega

theorem reopen_conserved (held capacity : Nat) : conserved (reopen held capacity) := by
  unfold reopen conserved; simp; omega

theorem registerRetired_conserved (l : State) (h : conserved l) :
    conserved (registerRetired l) := by
  unfold registerRetired conserved at *; split <;> simp <;> omega

/-- Registration admits only without debt, so an admission leaves held
permits within capacity. -/
theorem register_admits_within_capacity (l : State) (h : conserved l)
    (h_admit : l.held < (register l).held) : (register l).held ≤ l.capacity := by
  unfold register conserved at *
  split_ifs at h_admit ⊢
  all_goals (try simp only at h_admit ⊢)
  all_goals omega

/-- Debt forces zero availability through every ledger-owned step. -/
def debtBlocks (l : State) : Prop := 0 < l.owed → l.available = 0

theorem resize_debt_blocks (l : State) (capacity : Nat) (h : debtBlocks l) :
    debtBlocks (resize l capacity) := by
  unfold resize debtBlocks at *; split <;> simp <;> omega

theorem release_debt_blocks (l : State) (h : debtBlocks l) : debtBlocks (release l) := by
  unfold release debtBlocks at *; split
  · split <;> simp <;> omega
  · exact h

theorem reopen_debt_blocks (held capacity : Nat) : debtBlocks (reopen held capacity) := by
  unfold reopen debtBlocks; simp; omega

/-- The Tokio bypass: dropping a waiter that already holds a permit can make
a permit available while debt is outstanding, which is why registration, not
the semaphore, is the admission point. -/
theorem abandon_can_expose_permit_under_debt :
    ¬ debtBlocks (abandon ⟨1, 1, 0, 1, 1⟩) := by
  simp [debtBlocks, abandon]

end Ledger

end InferenceCall.Registry
