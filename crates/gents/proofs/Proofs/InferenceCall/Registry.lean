import Proofs.InferenceCall.ControllerBookkeeping

/-! One backend's admission capacity pool (#897, #1366).

`key` is the canonical resource configuration, including queue capacity and
credential identity but excluding display/catalog metadata. Rust's
queue-depth equality is therefore part of key equality, not an omitted
admission parameter. The keyed digest is a runtime representation of this
identity, not a cryptographic theorem here.

A backend document rewrite replaces the admitting controller incarnation
immediately; it never waits for earlier incarnations to drain. Every
incarnation of an open pool shares that pool's permits, so calls admitted
under a retired incarnation keep counting against the backend's capacity
until they release, and a capacity decrease blocks new admissions until the
excess releases. Serial drain made the backend unroutable for the whole drain
window, and indefinitely when a retired call never returned (#897, #1366).

Unavailability and removal close the pool. Permits outstanding at close are
abandoned rather than counted against a later pool: availability returns only
through a fresh probe/configuration decision about the backend, and the calls
admitted before it went away belong to the backend that failed. `held` counts
real permits, not persisted InferenceCall rows. -/
namespace InferenceCall.Registry

structure Config where
  key : Nat
  /-- Observed runtime epoch; rollback may reuse it. Attribution only: it is
  never a replacement trigger and never identifies a pool. -/
  generation : Nat
  capacity : Nat
  available : Bool
  deriving DecidableEq, Repr

/-- `admitting` is the incarnation that admits new calls; `none` means the
pool is closed. `pool` identifies the current pool and changes only when an
available configuration opens a fresh pool after a closed one. `held` is the
number of permits held against the open pool by every incarnation sharing it.
Keeping `desired` separate from `admitting` records the latest snapshot even
when it only changes metadata. -/
structure State where
  desired : Option Config
  admitting : Option Config
  pool : Nat
  held : Nat
  deriving DecidableEq, Repr

def availableDesired (desired : Option Config) : Option Config :=
  desired.filter (·.available)

/-- Key and capacity jointly characterize admission resources; generation is
not a replacement trigger. A metadata-only runtime generation keeps its owner. -/
def sameResources (a b : Config) : Bool :=
  a.key == b.key && a.capacity == b.capacity

def reconcile (s : State) (desired : Option Config) : State :=
  match availableDesired desired, s.admitting with
  | none, _ => ⟨desired, none, s.pool, 0⟩
  | some next, none => ⟨desired, some next, s.pool + 1, 0⟩
  | some next, some current =>
      if sameResources current next then { s with desired := desired }
      else { s with desired := desired, admitting := some next }

def capacity (s : State) : Nat :=
  (s.admitting.map (·.capacity)).getD 0

/-- New calls are admitted under the admitting incarnation while the pool's
held permits, including those of retired incarnations, are below the current
capacity. -/
def acquire (s : State) : Option State :=
  match s.admitting with
  | none => none
  | some c => if s.held < c.capacity then some { s with held := s.held + 1 } else none

/-- A permit returns to the pool it was taken from. Permits of a closed or
superseded pool are abandoned and cannot free capacity in the current one. -/
def release (s : State) (pool : Nat) : State :=
  if s.admitting.isSome && pool == s.pool && 0 < s.held then { s with held := s.held - 1 } else s

theorem metadata_only_reconcile_preserves_admitting
    (s : State) (desired : Option Config) (current next : Config)
    (h_admitting : s.admitting = some current)
    (h_available : availableDesired desired = some next)
    (h_same : sameResources current next = true) :
    reconcile s desired = { s with desired := desired } := by
  simp [reconcile, h_admitting, h_available, h_same]

/-- #1366: a resource rewrite of an available backend hands new admissions to
the replacement at once, in the same pool and with every earlier permit still
counted. There is no state in which the backend exists but admits nothing. -/
theorem rewrite_replaces_admitting_without_gap
    (s : State) (desired : Option Config) (current next : Config)
    (h_admitting : s.admitting = some current)
    (h_available : availableDesired desired = some next)
    (h_changed : sameResources current next = false) :
    (reconcile s desired).admitting = some next ∧
      (reconcile s desired).pool = s.pool ∧
      (reconcile s desired).held = s.held := by
  simp [reconcile, h_admitting, h_available, h_changed]

/-- An available desired configuration always leaves an admitting incarnation
with the desired capacity, whatever the prior state (#897: no retired call can
keep the backend closed). -/
theorem available_desired_admits (s : State) (desired : Option Config) (next : Config)
    (h_available : availableDesired desired = some next) :
    ∃ c, (reconcile s desired).admitting = some c ∧ c.capacity = next.capacity := by
  cases h : s.admitting with
  | none => exact ⟨next, by simp [reconcile, h_available, h], rfl⟩
  | some current =>
      cases hs : sameResources current next
      · exact ⟨next, by simp [reconcile, h_available, h, hs], rfl⟩
      · refine ⟨current, by simp [reconcile, h_available, h, hs], ?_⟩
        simp [sameResources] at hs
        exact hs.2

theorem acquire_below_capacity_admits (s : State) (c : Config)
    (h_admitting : s.admitting = some c) (h_free : s.held < c.capacity) :
    acquire s = some { s with held := s.held + 1 } := by
  simp [acquire, h_admitting, h_free]

/-- New admissions never raise the pool's held permits above the current
capacity (S7 across controller incarnations). -/
theorem acquire_within_capacity (s post : State) (h : acquire s = some post) :
    post.admitting = s.admitting ∧ post.held ≤ capacity post := by
  cases hc : s.admitting with
  | none => simp [acquire, hc] at h
  | some c =>
      simp only [acquire, hc] at h
      split at h
      · rename_i hlt
        simp only [Option.some.injEq] at h
        subst post
        simp [capacity, hc]
        omega
      · simp at h

/-- After a capacity decrease, calls admitted earlier keep their permits, and
nothing new is admitted until the excess has released. -/
theorem over_capacity_blocks_acquire (s : State) (h : capacity s ≤ s.held) :
    acquire s = none := by
  cases hc : s.admitting with
  | none => simp [acquire, hc]
  | some c =>
      simp [capacity, hc] at h
      simp [acquire, hc, Nat.not_lt.mpr h]

theorem closed_pool_never_admits (s : State) (h : s.admitting = none) :
    acquire s = none := by
  simp [acquire, h]

/-- Deleting or disabling a backend closes admission for new calls. -/
theorem unavailable_blocks_acquire (s : State) (desired : Option Config)
    (h : availableDesired desired = none) :
    acquire (reconcile s desired) = none := by
  simp [reconcile, h, acquire]

/-- A late release of a removed backend's permit cannot reopen admission. -/
theorem removed_backend_cannot_be_resurrected (s : State) (pool : Nat) :
    (release (reconcile s none) pool).admitting = none := by
  simp [reconcile, availableDesired, release]

/-- Reopening after unavailability starts a fresh pool; permits of the closed
pool cannot free its capacity. -/
theorem reopened_pool_ignores_prior_permits (s : State) (desired : Option Config)
    (next : Config) (h_closed : s.admitting = none)
    (h_available : availableDesired desired = some next) :
    release (reconcile s desired) s.pool = reconcile s desired := by
  simp [reconcile, h_closed, h_available, release]

theorem different_pool_release_stutters (s : State) (pool : Nat) (h : pool ≠ s.pool) :
    release s pool = s := by
  simp [release, h]

/-- Retired and current permits drain through the same finite count: each
matched release strictly decreases it. -/
theorem release_decreases_held (s : State) (h_open : s.admitting.isSome = true)
    (h_held : 0 < s.held) :
    (release s s.pool).held = s.held - 1 ∧ s.held - 1 < s.held := by
  simp [release, h_open, h_held]
  omega

/-- Held permits above the current capacity; only a capacity decrease raises it. -/
def excess (s : State) : Nat := s.held - capacity s

theorem acquire_leaves_no_excess (s post : State) (h : acquire s = some post) :
    excess post = 0 := by
  have := (acquire_within_capacity s post h).2
  unfold excess
  omega

theorem release_does_not_raise_excess (s : State) (pool : Nat) :
    excess (release s pool) ≤ excess s := by
  unfold release excess capacity
  split
  · simp
    omega
  · exact Nat.le_refl _

theorem reconcile_retains_latest_desired (s : State) (desired : Option Config) :
    (reconcile s desired).desired = desired := by
  unfold reconcile
  split
  · rfl
  · rfl
  · split <;> rfl

end InferenceCall.Registry
