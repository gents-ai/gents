import Proofs.BackendHealth.State

/-!
# Backend Health — Transitions

Deterministic, **total** event-driven transitions. There is no removal event
(a backend deleted from the registry simply stops being probed; its local
entry is dropped by the cycle's retain step, outside this machine).

`Threshold` is the consecutive-failure threshold K (default 3 in Rust).
-/

namespace Proofs.BackendHealth

/-- Consecutive-failure threshold. K=3 is the production default. -/
abbrev Threshold := { k : Nat // k ≥ 1 }

namespace Threshold

def one : Threshold := ⟨1, Nat.le.refl⟩

def ofNat (k : Nat) (h : k ≥ 1) : Threshold := ⟨k, h⟩

end Threshold

/-- One transition step.

    `probeSuccess` promotes to `.healthy` and resets `failureCount` — a
    single success recovers routing regardless of prior state.
    `probeFail` increments `failureCount` and demotes to `.unhealthy` once
    the new count reaches K, else `.degraded`. -/
def step (m : Model) (e : Event) (K : Threshold) : Model :=
  match e with
  | .probeSuccess => { state := .healthy, failureCount := 0 }
  | .probeFail =>
      let n := m.failureCount + 1
      if n ≥ K.val then { state := .unhealthy, failureCount := n }
      else { state := .degraded, failureCount := n }

/-- Sequential application of events. Total — no short-circuit. -/
def run (m : Model) (events : List Event) (K : Threshold) : Model :=
  events.foldl (fun acc e => step acc e K) m

@[simp]
theorem run_nil (m : Model) (K : Threshold) : run m [] K = m := rfl

theorem run_cons (m : Model) (e : Event) (rest : List Event) (K : Threshold) :
    run m (e :: rest) K = run (step m e K) rest K := rfl

theorem run_append (m : Model) (a b : List Event) (K : Threshold) :
    run m (a ++ b) K = run (run m a K) b K := by
  simp [run, List.foldl_append]

end Proofs.BackendHealth
