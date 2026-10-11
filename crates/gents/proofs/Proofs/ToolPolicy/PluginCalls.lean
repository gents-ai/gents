import Mathlib.Data.Nat.Basic

namespace ToolPolicy.PluginCalls

/-- Tool delegation is an explicit grant on the installed plugin's selected
Tools entry, observed by the native call owner rather than taken from guest
JSON. Only the owned loop supplies a live dispatcher. -/
def available (selectedGrant liveDispatcher : Bool) : Bool :=
  selectedGrant && liveDispatcher

theorem requires_grant (liveDispatcher : Bool) :
    available false liveDispatcher = false := by simp [available]

/-- The host dispatches every child without a delegation service, so a child
that is itself an installed plugin, including the same plugin, sees no live
dispatcher and cannot mint grandchildren. -/
def childDispatcher : Bool := false

theorem children_cannot_delegate (selectedGrant : Bool) :
    available selectedGrant childDispatcher = false := by
  simp [available, childDispatcher]

def maxEffects : Nat := 64
def maxPayloadBytes : Nat := 1048576

/-- How the session hook owns a tool call before ordinary dispatch. Hook-owned
calls may commit their effect, detach, await or terminate the loop inside the
before hook; a finite plugin invocation cannot own those transitions, so only
ordinary dispatch is child-callable and any newly hook-owned route is refused
by construction. -/
inductive DispatchRoute where
  | ordinary
  | hookOwned
  deriving DecidableEq, Repr

def supported : DispatchRoute → Bool
  | .ordinary => true
  | .hookOwned => false

/-- A child runs only when it is on the parent's selected surface, ordinary
dispatch owns it, and that tool's own admission accepts its arguments. -/
def childAdmissible (selected : Bool) (route : DispatchRoute) (admitted : Bool) : Bool :=
  selected && supported route && admitted

theorem admissible_child_is_selected (route : DispatchRoute) (admitted : Bool) :
    childAdmissible false route admitted = false := by simp [childAdmissible]

theorem hook_owned_child_refused (selected admitted : Bool) :
    childAdmissible selected .hookOwned admitted = false := by
  simp [childAdmissible, supported]

/-- The host charges attempted calls before dispatch. Refusals consume a slot
too, so repeated rejected requests cannot evade the per-invocation ceiling. -/
def reserve (used : Nat) : Option Nat :=
  if used < maxEffects then some (used + 1) else none

def resultFits (bytes : Nat) : Bool := bytes ≤ maxPayloadBytes

theorem reserved_bounded (used next : Nat) (h : reserve used = some next) :
    next ≤ maxEffects := by
  simp only [reserve] at h
  split at h
  next hlt => cases h; omega
  next => contradiction

/-- What the dispatcher observed for one child. `timedOut` and `cancelled`
are the child's own managed envelope; `fatal` is a durable settlement failure
or a hook termination decision. -/
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

/-- A child's interruption is terminal for that child only: the host records
its terminal state and returns an error to the guest. The parent ends through
its own deadline or cancellation envelope, never by inheriting a child's
managed terminal. -/
def settle : ChildOutcome → Settlement
  | .completed => .result
  | .failed | .unavailable | .timedOut | .cancelled => .guestError
  | .fatal => .parentFatal

theorem child_interruption_is_child_scoped :
    settle .timedOut = .guestError ∧ settle .cancelled = .guestError := ⟨rfl, rfl⟩

theorem only_fatal_stops_parent (outcome : ChildOutcome) :
    settle outcome = .parentFatal ↔ outcome = .fatal := by
  cases outcome <;> simp [settle]

/-- One served round: guest compute time and the host time serving its
children. The plugin's wall-clock budget bounds guest compute only; children
are bounded by the parent tool call's own deadline and cancellation. -/
def chargedCompute (rounds : List (Nat × Nat)) : Nat :=
  (rounds.map Prod.fst).sum

theorem serving_children_is_not_charged (rounds : List (Nat × Nat)) (served : Nat → Nat) :
    chargedCompute (rounds.map fun round => (round.1, served round.2)) =
      chargedCompute rounds := by
  simp [chargedCompute, Function.comp_def]

/-- Path consent waits for the operator only for an interactive caller. A
composed child cannot suspend its parent invocation for consent, so it is
refused up front with a reason that names the composition. -/
inductive ConsentRoute where
  | ask
  | refuseComposed
  | deny
  deriving DecidableEq, Repr

def consentRoute (interactive composed : Bool) : ConsentRoute :=
  if composed then .refuseComposed else if interactive then .ask else .deny

theorem composed_child_never_waits (interactive : Bool) :
    consentRoute interactive true = .refuseComposed := by simp [consentRoute]

end ToolPolicy.PluginCalls
