import Proofs.InferenceCall.ControllerBookkeeping

/-! One backend's serial-drain registry refinement. `key` is the canonical
resource configuration, including queue capacity and credential identity but
excluding display/catalog metadata. Rust's queue-depth equality is therefore
part of key equality, not an omitted admission parameter. The keyed digest is
a runtime representation of this identity, not a cryptographic theorem here.
Counts represent
real in-flight controller ownership, not persisted InferenceCall rows. -/
namespace InferenceCall.Registry

-- Queued ownership is supplied by InferenceCall.ControllerBookkeeping:
-- its inFlightContribution includes entered/queued/assigned/admitted phases,
-- while permitContribution counts only assigned/admitted permits. This model
-- starts from any coherent aggregate; it does not claim enqueue reachability.

structure Config where
  key : Nat
  /-- Observed runtime epoch; rollback may reuse it. Distinct controller
  incarnations with the same epoch are outside this numeric projection. -/
  generation : Nat
  capacity : Nat
  available : Bool
  deriving DecidableEq, Repr

structure Controller where
  config : Config
  inFlight : Nat
  permits : Nat
  deriving DecidableEq, Repr

/-- One controller exists at a time. `isOpen = false` means retiring. Keeping
latest desired independent of the controller prevents stale drain callbacks
from resurrecting a removed backend. -/
structure State where
  desired : Option Config
  controller : Option Controller
  isOpen : Bool
  deriving DecidableEq, Repr

def availableDesired (desired : Option Config) : Option Config :=
  desired.filter (·.available)

def install (desired : Option Config) : State :=
  match availableDesired desired with
  | none => ⟨desired, none, false⟩
  | some config => ⟨desired, some ⟨config, 0, 0⟩, true⟩

/-- Key and capacity jointly characterize reusable resources; generation is
not a replacement trigger. A metadata-only runtime generation keeps its owner. -/
def sameResources (a b : Config) : Bool :=
  a.key == b.key && a.capacity == b.capacity

def reconcile (s : State) (desired : Option Config) : State :=
  match s.controller with
  | none => install desired
  | some old =>
      if s.isOpen && (availableDesired desired).any (sameResources old.config) then
        { s with desired := desired }
      else if old.inFlight == 0 then install desired
      else ⟨desired, some old, false⟩

/-- The caller reports whether this real release returns a semaphore permit
or an unadmitted waiter. Reject impossible bookkeeping observations. -/
def canRelease (c : Controller) (returnedPermit : Bool) : Prop :=
  if returnedPermit then 0 < c.permits else c.permits < c.inFlight

instance (c : Controller) (returnedPermit : Bool) : Decidable (canRelease c returnedPermit) := by
  unfold canRelease
  infer_instance

/-- Final release installs only the latest desired configuration. -/
private def releaseCurrent (s : State) (returnedPermit : Bool) : State :=
  match s.controller with
  | none => s
  | some old =>
      if canRelease old returnedPermit then
        if !s.isOpen && old.inFlight == 1 then install s.desired
        else { s with controller := some { old with
          inFlight := old.inFlight - 1
          permits := old.permits - (if returnedPermit then 1 else 0) } }
      else s

/-- This projection rejects releases bearing a different runtime epoch.
Rust releases through the originating controller Arc, which also isolates
distinct controller incarnations when rollback reuses an epoch. That stronger
same-epoch ownership property is covered by the real rollback test. -/
def release (s : State) (generation : Nat) (returnedPermit : Bool) : State :=
  if s.controller.any (fun c => c.config.generation == generation) then
    releaseCurrent s returnedPermit
  else s

theorem different_epoch_release_stutters (s : State) (generation : Nat)
    (returnedPermit : Bool)
    (h : s.controller.any (fun c => c.config.generation == generation) = false) :
    release s generation returnedPermit = s := by
  simp [release, h]

def acquire (s : State) : Option State :=
  match s.controller with
  | none => none
  | some c =>
      if s.isOpen && c.permits < c.config.capacity then
        some { s with controller := some { c with inFlight := c.inFlight + 1, permits := c.permits + 1 } }
      else none

def capacityBound (s : State) : Prop :=
  ∀ c, s.controller = some c → c.permits ≤ c.config.capacity

theorem metadata_only_reconcile_preserves_active
    (previous desired : Option Config) (old : Controller)
    (h : (availableDesired desired).any (sameResources old.config) = true) :
    reconcile ⟨previous, some old, true⟩ desired = ⟨desired, some old, true⟩ := by
  simp [reconcile, h]

theorem closed_generation_never_admits (s : State) (h : s.isOpen = false) :
    acquire s = none := by
  cases hc : s.controller <;> simp [acquire, hc, h]

theorem replacement_waits_for_retiring_release
    (desired : Option Config) (old : Controller) (h_count : old.inFlight ≠ 0) :
    reconcile ⟨none, some old, false⟩ desired = ⟨desired, some old, false⟩ := by
  simp [reconcile, h_count]

theorem latest_available_config_installs_after_last_release
    (prior : Config) (desired : Config) (h_available : desired.available = true) :
    release ⟨some desired, some ⟨prior, 1, 1⟩, false⟩ prior.generation true =
      ⟨some desired, some ⟨desired, 0, 0⟩, true⟩ := by
  simp [release, releaseCurrent, canRelease, install, availableDesired, Option.filter, h_available]

theorem removed_backend_cannot_be_resurrected (old : Config) :
    release (reconcile ⟨some old, some ⟨old, 1, 1⟩, false⟩ none) old.generation true =
      ⟨none, none, false⟩ := by
  simp [reconcile, release, releaseCurrent, canRelease, install, availableDesired]

theorem unavailable_cannot_be_resurrected (old desired : Config)
    (h : desired.available = false) :
    release (reconcile ⟨some old, some ⟨old, 1, 1⟩, false⟩ (some desired)) old.generation true =
      ⟨some desired, none, false⟩ := by
  simp [reconcile, release, releaseCurrent, canRelease, install, availableDesired, Option.filter, h]

theorem install_capacity_bound (desired : Option Config) :
    capacityBound (install desired) := by
  unfold install
  split <;> simp [capacityBound]

theorem reconcile_preserves_capacity (s : State) (desired : Option Config)
    (h : capacityBound s) : capacityBound (reconcile s desired) := by
  cases hc : s.controller with
  | none => simpa [reconcile, hc] using install_capacity_bound desired
  | some old =>
      have ho := h old hc
      simp only [reconcile, hc]
      split
      · simpa [capacityBound, hc] using ho
      · split
        · exact install_capacity_bound desired
        · simpa [capacityBound] using ho

theorem release_preserves_capacity (s : State) (returnedPermit : Bool)
    (h : capacityBound s) : capacityBound (releaseCurrent s returnedPermit) := by
  cases hc : s.controller with
  | none => simpa [releaseCurrent, hc] using h
  | some old =>
      have ho := h old hc
      simp only [releaseCurrent, hc]
      split
      · split
        · exact install_capacity_bound s.desired
        · simp [capacityBound]
          omega
      · exact h

theorem acquire_preserves_capacity (s post : State)
    (h : acquire s = some post) : capacityBound post := by
  cases hc : s.controller with
  | none => simp [acquire, hc] at h
  | some old =>
      simp only [acquire, hc] at h
      split at h
      · rename_i hg
        simp only [Option.some.injEq] at h
        subst post
        simp [capacityBound]
        simp only [Bool.and_eq_true, decide_eq_true_eq] at hg
        omega
      · simp at h

/-- Queue waiters are allowed above capacity; actual permits are bounded and
never exceed the ownership count used to declare a controller drained. -/
def drainSafe (s : State) : Prop :=
  ∀ c, s.controller = some c → c.permits ≤ c.inFlight

theorem release_preserves_drain_safety (s : State) (returnedPermit : Bool)
    (h : drainSafe s) : drainSafe (releaseCurrent s returnedPermit) := by
  cases hc : s.controller with
  | none => simpa [releaseCurrent, hc] using h
  | some old =>
      have ho := h old hc
      simp only [releaseCurrent, hc]
      split
      · rename_i hr
        split
        · unfold install
          split <;> simp [drainSafe]
        · simp [drainSafe]
          cases returnedPermit <;> simp [canRelease] at hr ⊢ <;> omega
      · exact h

/-- A matched non-final release strictly decreases outstanding ownership;
no reconciliation action fabricates such a release. -/
theorem retiring_release_decreases (desired : Option Config) (old : Controller)
    (returnedPermit : Bool) (hr : canRelease old returnedPermit)
    (h : 1 < old.inFlight) :
    release ⟨desired, some old, false⟩ old.config.generation returnedPermit =
      ⟨desired, some { old with
        inFlight := old.inFlight - 1
        permits := old.permits - (if returnedPermit then 1 else 0) }, false⟩ ∧
      old.inFlight - 1 < old.inFlight := by
  have hone : old.inFlight ≠ 1 := by omega
  simp [release, releaseCurrent, hr, hone]
  omega

/-- One real owner returns per step, choosing a permit holder while any
permits remain, otherwise an unadmitted waiter. This is a finite witness of
release progress, not a claim that owners are scheduled or terminate. -/
def drainSteps : Nat → State → State
  | 0, s => s
  | n + 1, s =>
      let returned := s.controller.any (fun c => 0 < c.permits)
      let generation := (s.controller.map (·.config.generation)).getD 0
      drainSteps n (release s generation returned)

theorem finite_retiring_releases_install_latest
    (desired : Option Config) (config : Config) (count permits : Nat)
    (h : permits ≤ count + 1) :
    drainSteps (count + 1) ⟨desired, some ⟨config, count + 1, permits⟩, false⟩ =
      install desired := by
  induction count generalizing permits with
  | zero =>
      have hp : permits = 0 ∨ permits = 1 := by omega
      rcases hp with rfl | rfl <;> simp [drainSteps, release, releaseCurrent, canRelease]
  | succ count ih =>
      cases permits with
      | zero =>
          simpa [drainSteps, release, releaseCurrent, canRelease] using ih 0 (by omega)
      | succ permits =>
          simpa [drainSteps, release, releaseCurrent, canRelease] using ih permits (by omega)

theorem reconcile_retains_latest_desired (s : State) (desired : Option Config) :
    (reconcile s desired).desired = desired := by
  cases hc : s.controller with
  | none => simp only [reconcile, hc]; unfold install; split <;> rfl
  | some c =>
      simp only [reconcile, hc]
      split
      · rfl
      · split
        · unfold install; split <;> rfl
        · rfl

theorem attributed_release_preserves_capacity (s : State) (generation : Nat)
    (returnedPermit : Bool) (h : capacityBound s) :
    capacityBound (release s generation returnedPermit) := by
  unfold release
  split
  · exact release_preserves_capacity s returnedPermit h
  · exact h

theorem attributed_release_preserves_drain_safety (s : State) (generation : Nat)
    (returnedPermit : Bool) (h : drainSafe s) :
    drainSafe (release s generation returnedPermit) := by
  unfold release
  split
  · exact release_preserves_drain_safety s returnedPermit h
  · exact h

theorem install_drain_safe (desired : Option Config) : drainSafe (install desired) := by
  unfold install
  split <;> simp [drainSafe]

theorem reconcile_preserves_drain_safety (s : State) (desired : Option Config)
    (h : drainSafe s) : drainSafe (reconcile s desired) := by
  cases hc : s.controller with
  | none => simpa [reconcile, hc] using install_drain_safe desired
  | some old =>
      have ho := h old hc
      simp only [reconcile, hc]
      split
      · simpa [drainSafe, hc] using ho
      · split
        · exact install_drain_safe desired
        · simpa [drainSafe] using ho

theorem acquire_preserves_drain_safety (s post : State)
    (hs : drainSafe s) (h : acquire s = some post) : drainSafe post := by
  cases hc : s.controller with
  | none => simp [acquire, hc] at h
  | some old =>
      have ho := hs old hc
      simp only [acquire, hc] at h
      split at h
      · simp only [Option.some.injEq] at h
        subst post
        simp [drainSafe]
        omega
      · simp at h

/-- Real controller phase aggregates supply the registry drain invariant,
including queued and assigned-but-not-resumed owners. Enqueue/grant legality
remains owned by ControllerBookkeeping.step?. -/
theorem controller_bookkeeping_supplies_drain_safety
    (desired : Option Config) (config : Config) (isOpen : Bool)
    (ids : Finset Nat) (phase : Nat → ControllerBookkeeping.AdmissionPhase) :
    drainSafe ⟨desired, some ⟨config,
      ControllerBookkeeping.controllerInFlight ids phase,
      ControllerBookkeeping.controllerPermits ids phase⟩, isOpen⟩ := by
  intro c hc
  cases Option.some.inj hc
  exact Finset.sum_le_sum fun callId _ =>
    ControllerBookkeeping.permit_implies_in_flight (phase callId)

/-- A release with a different epoch leaves the replacement unchanged even
when its resource key and capacity match. This does not assert epoch freshness. -/
theorem different_epoch_release_after_replacement_case :
    release ⟨some ⟨7, 2, 2, true⟩,
      some ⟨⟨7, 2, 2, true⟩, 1, 1⟩, true⟩ 1 true =
      ⟨some ⟨7, 2, 2, true⟩, some ⟨⟨7, 2, 2, true⟩, 1, 1⟩, true⟩ := by
  decide

end InferenceCall.Registry
