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

end ToolPolicy.PluginCalls
