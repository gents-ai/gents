import Proofs.RequestExecutionLease.Transition

namespace RequestExecutionLease

/-- A native transaction admits a decision against its snapshot before attempting
its durable commit. `step?` remains the sole authorization policy. The stored
world here represents the request facts validated by point reads; it does not
assert that predicate scans are serializable or that native commit checks time. -/
structure ObservedTransaction (Generation : Type) where
  before : World Generation
  admitted : World Generation
  deriving DecidableEq, Repr

def observeTransaction? {Generation : Type} [DecidableEq Generation]
    (pre : World Generation) (action : Action Generation) :
    Option (ObservedTransaction Generation) :=
  (step? pre action).map fun post => ⟨pre, post⟩

/-- Native OCC rejects a changed authoritative request observation. Wall clock
passage alone does not change the stored request and therefore does not cause a
storage conflict. In particular, admission before expiry is not a promise that
commit finishes before expiry. The pinned native adapter must demonstrate the
request point read; a same-value write or an arbitrary scan is insufficient. -/
def commitObservedTransaction? {Generation : Type} [DecidableEq Generation]
    (current : World Generation) (observed : ObservedTransaction Generation) :
    Option (World Generation) :=
  if observed.before.now ≤ current.now ∧
      current.request = observed.before.request ∧ current.lease = observed.before.lease then
    some { observed.admitted with now := current.now }
  else none

theorem changed_observation_rejects_commit {Generation : Type} [DecidableEq Generation]
    (current : World Generation) (observed : ObservedTransaction Generation)
    (changed : current.request ≠ observed.before.request ∨
      current.lease ≠ observed.before.lease) :
    commitObservedTransaction? current observed = none := by
  rcases changed with changed | changed
  · simp [commitObservedTransaction?, changed]
  · simp [commitObservedTransaction?, changed]

end RequestExecutionLease
