import Proofs.MCPHealth.State

/-!
# MCP Health / Eviction — Transitions

Deterministic event-driven transitions. `step?` is total over `Event`; it
returns `none` only on `registryAbsent` (service removed) and `some sm'`
otherwise. `run?` short-circuits on the first `registryAbsent`.

`Threshold` is the failure-count threshold K. K=1 collapses to today's Rust
behavior (single probeFail evicts); K ≥ 2 admits the bounded-flap regime.
-/

namespace Proofs.MCPHealth

/-- Failure-count threshold. K=1 today; K ≥ 2 admits the flapping-bound regime. -/
abbrev Threshold := { k : Nat // k ≥ 1 }

namespace Threshold

/-- K=1: today's Rust behavior (single probeFail → Evicted). -/
def one : Threshold := ⟨1, Nat.le.refl⟩

/-- Lift an arbitrary `Nat ≥ 1` to a `Threshold` (helpful in conformance). -/
def ofNat (k : Nat) (h : k ≥ 1) : Threshold := ⟨k, h⟩

end Threshold

/-- One transition step.

    `registryAbsent` returns `none` — the per-service state machine ends.
    `backoffExpiry` is a no-op outside `.evicted`.
    `probeSuccess stale` resets `failureCount` to 0 and routes to `.healthy`
    (fresh) or `.degraded` (stale).
    `probeFail` increments `failureCount` and routes to `.evicted` if the
    new count ≥ K, else `.degraded`. -/
def step? (sm : ServiceModel) (e : Event) (K : Threshold) : Option ServiceModel :=
  match e with
  | .registryAbsent => none
  | .backoffExpiry  =>
      some { sm with state := if sm.state = .evicted then .reconnecting else sm.state }
  | .probeSuccess stale =>
      some { sm with
                state := if stale then .degraded else .healthy
              , failureCount := 0 }
  | .probeFail =>
      let n := sm.failureCount + 1
      if n ≥ K.val then some { sm with state := .evicted,  failureCount := n }
                   else some { sm with state := .degraded, failureCount := n }

/-- Sequential application of events. Short-circuits on `registryAbsent`. -/
def run? (sm : ServiceModel) (events : List Event) (K : Threshold)
    : Option ServiceModel :=
  events.foldl (fun acc e => acc.bind (fun sm' => step? sm' e K)) (some sm)

/-- `run? sm [] K = some sm`. -/
@[simp]
theorem run?_nil (sm : ServiceModel) (K : Threshold) :
    run? sm [] K = some sm := rfl

/-- One-event `run?` reduces to `step?`. -/
@[simp]
theorem run?_singleton (sm : ServiceModel) (e : Event) (K : Threshold) :
    run? sm [e] K = step? sm e K := by
  simp [run?, List.foldl, Option.bind]

/-- Compose two runs: `run? sm (a ++ b) K = (run? sm a K).bind (run? · b K)`.
    Used by `Properties.lean` to slice runs at intermediate prefixes. -/
-- Helper: foldl bind factors over append for any starting accumulator.
private theorem foldl_bind_append (acc : Option ServiceModel) (a b : List Event) (K : Threshold) :
    (a ++ b).foldl (fun acc e => acc.bind (fun sm' => step? sm' e K)) acc =
    (a.foldl (fun acc e => acc.bind (fun sm' => step? sm' e K)) acc).bind
      (fun sm' => b.foldl (fun acc e => acc.bind (fun sm' => step? sm' e K)) (some sm')) := by
  induction a generalizing acc with
  | nil =>
      simp only [List.foldl, List.nil_append, Option.bind]
      cases acc with
      | none =>
          induction b with
          | nil => rfl
          | cons _ _ ihb => simp [List.foldl, ihb]
      | some sm => rfl
  | cons e rest ih =>
      simp only [List.foldl, List.cons_append]
      rw [ih]

theorem run?_append (sm : ServiceModel) (a b : List Event) (K : Threshold) :
    run? sm (a ++ b) K = (run? sm a K).bind (fun sm' => run? sm' b K) := by
  simp only [run?]
  exact foldl_bind_append (some sm) a b K

/-- Helper: `run?` unfolds across `cons` via `step?` and a recursive `run?`. -/
theorem run?_cons (sm : ServiceModel) (e : Event) (rest : List Event) (K : Threshold) :
    run? sm (e :: rest) K = (step? sm e K).bind (fun sm'' => run? sm'' rest K) := by
  have := run?_append sm [e] rest K
  simpa using this

end Proofs.MCPHealth
