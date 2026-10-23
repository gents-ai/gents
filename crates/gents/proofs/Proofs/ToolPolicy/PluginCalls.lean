import Mathlib.Data.Nat.Basic

namespace ToolPolicy.PluginCalls

def available (selectedGrant liveDispatcher : Bool) : Bool :=
  selectedGrant && liveDispatcher

theorem requires_grant (liveDispatcher : Bool) :
    available false liveDispatcher = false := by simp [available]

def childDispatcher : Bool := false

theorem children_cannot_delegate (selectedGrant : Bool) :
    available selectedGrant childDispatcher = false := by
  simp [available, childDispatcher]

def maxEffects : Nat := 64
def maxPayloadBytes : Nat := 1048576

inductive DispatchRoute where
  | ordinary
  | hookOwned
  deriving DecidableEq, Repr

def supported : DispatchRoute → Bool
  | .ordinary => true
  | .hookOwned => false

def childAdmissible (selected : Bool) (route : DispatchRoute) (admitted : Bool) : Bool :=
  selected && supported route && admitted

theorem admissible_child_is_selected (route : DispatchRoute) (admitted : Bool) :
    childAdmissible false route admitted = false := by simp [childAdmissible]

theorem hook_owned_child_refused (selected admitted : Bool) :
    childAdmissible selected .hookOwned admitted = false := by
  simp [childAdmissible, supported]

/-- Job-bound tool names are hook-owned: the hook adopts the call (flip, pin,
effects) before any finite invocation could own it. Rust's
`tool_calls::supported` consults the hook's job-tool set, because an
operator-chosen name cannot be in the static table. -/
def routeOf (staticHookOwned jobBound : Bool) : DispatchRoute :=
  if staticHookOwned || jobBound then .hookOwned else .ordinary

theorem job_tool_not_composable (selected staticHookOwned admitted : Bool) :
    childAdmissible selected (routeOf staticHookOwned true) admitted = false := by
  simpa [routeOf] using hook_owned_child_refused selected admitted

def reserve (used : Nat) : Option Nat :=
  if used < maxEffects then some (used + 1) else none

def resultFits (bytes : Nat) : Bool := bytes ≤ maxPayloadBytes

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
