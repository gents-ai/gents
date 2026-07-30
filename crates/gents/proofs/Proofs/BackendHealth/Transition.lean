import Proofs.BackendHealth.State

namespace Proofs.BackendHealth

abbrev Threshold := { k : Nat // k ≥ 1 }

namespace Threshold

def one : Threshold := ⟨1, Nat.le.refl⟩

def ofNat (k : Nat) (h : k ≥ 1) : Threshold := ⟨k, h⟩

end Threshold

def step (m : Model) (e : Event) (K : Threshold) : Model :=
  match e with
  | .probeSuccess => { state := .healthy, failureCount := 0 }
  | .probeFail =>
      let n := m.failureCount + 1
      if n ≥ K.val then { state := .unhealthy, failureCount := n }
      else { state := .degraded, failureCount := n }

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
