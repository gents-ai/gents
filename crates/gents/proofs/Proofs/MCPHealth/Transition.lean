import Proofs.MCPHealth.State

namespace Proofs.MCPHealth

abbrev Threshold := { k : Nat // k ≥ 1 }

namespace Threshold

def one : Threshold := ⟨1, Nat.le.refl⟩

def ofNat (k : Nat) (h : k ≥ 1) : Threshold := ⟨k, h⟩

end Threshold

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

def run? (sm : ServiceModel) (events : List Event) (K : Threshold)
    : Option ServiceModel :=
  events.foldl (fun acc e => acc.bind (fun sm' => step? sm' e K)) (some sm)

@[simp]
theorem run?_nil (sm : ServiceModel) (K : Threshold) :
    run? sm [] K = some sm := rfl

@[simp]
theorem run?_singleton (sm : ServiceModel) (e : Event) (K : Threshold) :
    run? sm [e] K = step? sm e K := by
  simp [run?, List.foldl, Option.bind]

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

theorem run?_cons (sm : ServiceModel) (e : Event) (rest : List Event) (K : Threshold) :
    run? sm (e :: rest) K = (step? sm e K).bind (fun sm'' => run? sm'' rest K) := by
  have := run?_append sm [e] rest K
  simpa using this

end Proofs.MCPHealth
