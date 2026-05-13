import Proofs.MCPHealth.Transition

/-!
# MCP Health / Eviction — Properties

Safety, arithmetic, and liveness facts about `step?` / `run?`. Property
tags (H1–H8, H6') match the spec table in §8 of
`docs/superpowers/specs/2026-05-13-mcp-health-lean-design.md`.

This file is built additively. Task 4 adds the easy safety/arithmetic facts;
later tasks add H7 (K=1 collapse), H6 / H6' (liveness), and H5 (anti-flapping
inter-eviction gap, load-bearing).
-/

namespace Proofs.MCPHealth

/-- H1: every legal next-state arises from a named `Event`; no spontaneous
    transitions. Trivially structural — `step?` is a total function of
    `Event`. Recorded as a fact for the audit's "no spontaneous transitions"
    acceptance criterion. -/
theorem h1_event_triggered
    (sm sm' : ServiceModel) (K : Threshold) :
    (∃ e, step? sm e K = some sm') ↔
      sm' ∈ (Event.all.filterMap (fun e => step? sm e K)) := by
  constructor
  · rintro ⟨e, he⟩
    have := Event.all_complete e
    simp [List.mem_filterMap]
    exact ⟨e, this, he⟩
  · intro h
    simp [List.mem_filterMap] at h
    obtain ⟨e, _, he⟩ := h
    exact ⟨e, he⟩

/-- H2: `probeSuccess` resets `failureCount` to 0. -/
@[simp]
theorem h2_success_resets_failure_count
    (sm : ServiceModel) (stale : Bool) (K : Threshold) :
    (step? sm (.probeSuccess stale) K).map (·.failureCount) = some 0 := rfl

/-- H3: `probeFail` increments `failureCount` by exactly 1. -/
@[simp]
theorem h3_probefail_increments_count
    (sm : ServiceModel) (K : Threshold) :
    (step? sm .probeFail K).map (·.failureCount) = some (sm.failureCount + 1) := by
  simp only [step?]
  split <;> simp

/-- H4: `backoffExpiry` only changes state when starting from `.evicted`. -/
@[simp]
theorem h4_backoff_only_from_evicted (sm : ServiceModel) (K : Threshold) :
    (step? sm .backoffExpiry K).map (·.state)
      = some (if sm.state = .evicted then .reconnecting else sm.state) := rfl

/-- H8: `registryAbsent` ends the per-service state machine. -/
@[simp]
theorem h8_registry_absent_terminates
    (sm : ServiceModel) (K : Threshold) :
    step? sm .registryAbsent K = none := rfl

end Proofs.MCPHealth
