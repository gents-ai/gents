import Mathlib.Data.Nat.Basic
import Proofs.Session.ToolPolicy.PluginCallLimits

namespace ToolPolicy.PluginCalls

def available (selectedGrant liveDispatcher : Bool) : Bool :=
  selectedGrant && liveDispatcher

theorem requires_grant (liveDispatcher : Bool) :
    available false liveDispatcher = false := by simp [available]

/-- What started an invocation. The model calls a tool directly or backgrounds
it through its own `spawn_process`; a plugin calls a child, which may itself be
`spawn_process`. -/
inductive Origin where
  | accepted
  | spawnedByAccepted
  | pluginChild
  | spawnedByPluginChild
  deriving DecidableEq, Repr

/-- Only an invocation the model started receives a tool dispatcher. A child
receives none, and neither does the background invocation a child spawns:
with a dispatcher it would compose under a fresh `maxEffects` budget and
could spawn its like in turn, so delegation would be unbounded in depth. -/
def dispatcherFor : Origin → Bool
  | .accepted | .spawnedByAccepted => true
  | .pluginChild | .spawnedByPluginChild => false

def childDispatcher : Bool := dispatcherFor .pluginChild

theorem children_cannot_delegate (selectedGrant : Bool) :
    available selectedGrant childDispatcher = false := by
  simp [available, childDispatcher, dispatcherFor]

theorem spawned_by_child_cannot_delegate (selectedGrant : Bool) :
    available selectedGrant (dispatcherFor .spawnedByPluginChild) = false := by
  simp [available, dispatcherFor]

/-- Every composing invocation was started by the model, so composition depth
is one whatever a script spawns. -/
theorem delegation_requires_model_origin (selectedGrant : Bool) (origin : Origin)
    (h : available selectedGrant (dispatcherFor origin) = true) :
    origin = .accepted ∨ origin = .spawnedByAccepted := by
  cases origin <;> simp_all [available, dispatcherFor]

inductive DispatchRoute where
  | ordinary
  | hookOwned
  deriving DecidableEq, Repr

def supported : DispatchRoute → Bool
  | .ordinary => true
  | .hookOwned => true

def childAdmissible (selected : Bool) (route : DispatchRoute) (admitted : Bool) : Bool :=
  selected && supported route && admitted

theorem admissible_child_is_selected (route : DispatchRoute) (admitted : Bool) :
    childAdmissible false route admitted = false := by simp [childAdmissible]

theorem hook_owned_child_requires_admission (selected : Bool) :
    childAdmissible selected .hookOwned false = false := by
  simp [childAdmissible, supported]

/-- Hook-owned tools retain their own admission and lifecycle decisions when
called by a plugin. A script never receives the hook or replaces its decisions. -/
def routeOf (staticHookOwned jobBound : Bool) : DispatchRoute :=
  if staticHookOwned || jobBound then .hookOwned else .ordinary

theorem job_tool_requires_admission (selected staticHookOwned : Bool) :
    childAdmissible selected (routeOf staticHookOwned true) false = false := by
  simp [childAdmissible]

def reserve (used : Nat) : Option Nat :=
  if used < maxEffects then some (used + 1) else none

def resultFits (bytes : Nat) : Bool := bytes ≤ maxPayloadBytes

/-- Sleeping grants no tool authority and cannot extend the enclosing invocation.
The native adapter waits asynchronously while the sandbox keeps its live state. -/
def sleepAdmissible (milliseconds remaining : Nat) : Bool := milliseconds ≤ remaining

theorem sleep_cannot_extend_invocation (milliseconds remaining : Nat)
    (h : sleepAdmissible milliseconds remaining = true) : milliseconds ≤ remaining := by
  simpa [sleepAdmissible] using h

theorem reserved_bounded (used next : Nat) (h : reserve used = some next) :
    next ≤ maxEffects := by
  simp only [reserve] at h
  split at h
  next hlt => cases h; omega
  next => contradiction

inductive ChildOutcome where
  | completed
  | failed
  | unavailable
  | timedOut
  | cancelled
  | fatal
  deriving DecidableEq, Repr

inductive Settlement where
  | result
  | guestError
  | parentFatal
  deriving DecidableEq, Repr

def settle : ChildOutcome → Settlement
  | .completed => .result
  | .failed | .unavailable | .timedOut | .cancelled => .guestError
  | .fatal => .parentFatal

theorem child_interruption_is_child_scoped :
    settle .timedOut = .guestError ∧ settle .cancelled = .guestError := ⟨rfl, rfl⟩

theorem only_fatal_stops_parent (outcome : ChildOutcome) :
    settle outcome = .parentFatal ↔ outcome = .fatal := by
  cases outcome <;> simp [settle]

/-- A background host-call plugin runs under the lifetime already admitted by
its tool execution owner. Foreground calls retain the declared guest ceiling. -/
def hostCallLifetime (declared : Nat) (background : Bool) (remaining : Option Nat) : Nat :=
  match remaining with
  | none => declared
  | some available => if background then available else min declared available

theorem background_host_call_uses_owner_lifetime (declared available : Nat) :
    hostCallLifetime declared true (some available) = available := rfl

theorem foreground_host_call_is_bounded (declared : Nat) (remaining : Option Nat) :
    hostCallLifetime declared false remaining ≤ declared := by
  cases remaining <;> simp [hostCallLifetime, Nat.min_le_left]

/-- The compute a host-call guest may spend before its next host call. The
sandbox exposes a guest's store only inside a host import, so the host renews
this allowance at each call rather than preempting on demand: a guest computes
for at most its declared wall clock between calls, never past the invocation's
end, and not at all once the invocation stopped. A background guest's admitted
lifetime (`hostCallLifetime`) therefore bounds its awaits, not its compute. -/
def computeAllowance (declared remaining : Nat) (stopped : Bool) : Nat :=
  if stopped then 0 else min declared remaining

theorem stopped_guest_has_no_compute (declared remaining : Nat) :
    computeAllowance declared remaining true = 0 := rfl

theorem compute_allowance_is_bounded (declared remaining : Nat) (stopped : Bool) :
    computeAllowance declared remaining stopped ≤ declared ∧
      computeAllowance declared remaining stopped ≤ remaining := by
  cases stopped <;> simp [computeAllowance, Nat.min_le_left, Nat.min_le_right]

/-- The store's epoch deadline that carries a compute allowance: whole tick
periods, rounded up, so the guest never gets less than its allowance and at
most one period more. A zero allowance is a deadline of the current epoch,
which traps at the guest's next epoch check.

External premise (afterburner's shared epoch VM, pinned by Gents): the epoch
deadline is a number of ticks, and one process-wide ticker thread advances the
epoch by one each time its `period` sleep ends. A sleep ends no earlier than
its period and arbitrarily later under scheduling or timer coalescing, so the
allowance is counted in wake-ups, not in elapsed time (`wakeCountedEpoch`):
with every wake `late` past its period, `allowanceTicks a p` ticks last
`allowanceTicks a p * (p + late)`, unbounded in `late`. The stop is late, never
lost. The ticker owns the fix: advancing the epoch to the whole periods
elapsed since it started (`catchUpEpoch`) bounds the stop by the allowance plus
one period and one late wake, whatever every other wake did
(`tla/GuestAllowance.tla`, `MCGuestAllowance` and `MCGuestAllowanceWakeCounted`). -/
def allowanceTicks (allowance period : Nat) : Nat :=
  (allowance + period - 1) / period

theorem allowance_ticks_cover_the_allowance (allowance period : Nat) (h : 0 < period) :
    allowance ≤ allowanceTicks allowance period * period := by
  unfold allowanceTicks
  have := Nat.div_add_mod (allowance + period - 1) period
  have := Nat.mod_lt (allowance + period - 1) h
  rw [Nat.mul_comm]
  omega

theorem allowance_ticks_add_less_than_a_period (allowance period : Nat) (h : 0 < period) :
    allowanceTicks allowance period * period < allowance + period := by
  unfold allowanceTicks
  have := Nat.div_mul_le_self (allowance + period - 1) period
  omega

theorem no_allowance_is_no_ticks (period : Nat) : allowanceTicks 0 period = 0 := by
  unfold allowanceTicks
  rcases Nat.eq_zero_or_pos period with h | h
  · simp [h]
  · exact Nat.div_eq_of_lt (by omega)

/-- The shipped ticker: each wake adds one to the epoch, whenever it happens. -/
def wakeCountedEpoch (wakes : List Nat) : Nat := wakes.length

/-- The proposed ticker: a wake at `elapsed` (time since the ticker started)
sets the epoch to the whole periods elapsed, and never moves it back. -/
def catchUpEpoch (period : Nat) : List Nat → Nat
  | [] => 0
  | elapsed :: earlier => max (catchUpEpoch period earlier + 1) (elapsed / period)

/-- Counting wake-ups: wakes `period + late` apart reach epoch `n` only after
`n * (period + late)`, so lateness on every wake stretches an allowance by the
allowance's tick count times that lateness. -/
theorem wake_counted_epoch_ignores_elapsed_time (period late n : Nat) :
    wakeCountedEpoch ((List.range n).reverse.map fun k => (k + 1) * (period + late)) = n := by
  simp [wakeCountedEpoch]

/-- Catching up: after its latest wake the epoch is at least the whole
periods elapsed by then, however late any wake was. -/
theorem catch_up_epoch_tracks_elapsed_time (period elapsed : Nat) (earlier : List Nat) :
    elapsed / period ≤ catchUpEpoch period (elapsed :: earlier) :=
  le_max_right _ _

def chargedCompute (rounds : List (Nat × Nat)) : Nat :=
  (rounds.map Prod.fst).sum

theorem serving_children_is_not_charged (rounds : List (Nat × Nat)) (served : Nat → Nat) :
    chargedCompute (rounds.map fun round => (round.1, served round.2)) =
      chargedCompute rounds := by
  simp [chargedCompute, Function.comp_def]

inductive ConsentRoute where
  | ask
  | deny
  deriving DecidableEq, Repr

def consentRoute (interactive _composed : Bool) : ConsentRoute :=
  if interactive then .ask else .deny

/-- A plugin call composed inside another plugin's effects routes consent
exactly as a direct call does, so the runtime does not carry composition as an
input to consent. -/
theorem composed_child_waits_like_a_direct_call (interactive : Bool) :
    consentRoute interactive true = consentRoute interactive false := by
  simp [consentRoute]

theorem interactive_composed_child_is_asked : consentRoute true true = .ask := by
  simp [consentRoute]

/-- A bound path outside every allowed folder. A headless call never files a
question; a refused bind names the command that allows the folder, and an
interactive one says the operator declined it. -/
structure UncoveredBind where
  asked : Bool
  bound : Bool
  declinedNote : Bool
  allowHint : Bool
  deriving DecidableEq, Repr

def uncoveredBind (interactive operatorAllows : Bool) : UncoveredBind :=
  let asked := consentRoute interactive false == .ask
  let bound := asked && operatorAllows
  { asked, bound, declinedNote := !bound && interactive, allowHint := !bound }

theorem a_headless_call_never_asks (operatorAllows : Bool) :
    (uncoveredBind false operatorAllows).asked = false ∧
      (uncoveredBind false operatorAllows).bound = false ∧
      (uncoveredBind false operatorAllows).allowHint = true := by
  cases operatorAllows <;> decide

end ToolPolicy.PluginCalls
