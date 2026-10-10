import Mathlib.Data.Nat.Basic

namespace ToolPolicy.PluginCalls

/-- Tool delegation is an explicit grant on the installed plugin's selected
Tools entry. Only the owned loop supplies a live parent; nested tool calls do
not inherit delegation, including calls to the same installed plugin. -/
def available (selectedGrant liveParent nested : Bool) : Bool :=
  selectedGrant && liveParent && !nested

theorem requires_grant (liveParent nested : Bool) :
    available false liveParent nested = false := by simp [available]

theorem nested_has_no_delegation (selectedGrant liveParent : Bool) :
    available selectedGrant liveParent true = false := by simp [available]

def maxEffects : Nat := 64
def maxResultBytes : Nat := 1048576

/-- Request/session/process control hooks may detach work, terminate the loop,
or await before ordinary dispatch. A finite plugin invocation cannot own those
transitions; it must ask the model to call these tools directly. -/
def supported (name : String) : Bool :=
  !(["update_goal", "agent_new", "agent_message", "spawn_process", "wait_process"].contains name)

/-- The host charges attempted calls before dispatch. Refusals consume a slot
too, so repeated rejected requests cannot evade the per-invocation ceiling. -/
def reserve (used : Nat) : Option Nat :=
  if used < maxEffects then some (used + 1) else none

def resultFits (bytes : Nat) : Bool := bytes ≤ maxResultBytes

theorem reserved_bounded (used next : Nat) (h : reserve used = some next) :
    next ≤ maxEffects := by
  simp only [reserve] at h
  split at h
  next hlt => cases h; omega
  next => contradiction

end ToolPolicy.PluginCalls
