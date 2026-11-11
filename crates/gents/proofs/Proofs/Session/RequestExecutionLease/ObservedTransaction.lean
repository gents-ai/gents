import Proofs.RequestExecutionLease.Transition

namespace RequestExecutionLease

/-- A native transaction admits a decision against its snapshot before its
durable commit; `step?` remains the sole authorization policy, and the stored
world does not assert predicate scans are serializable or that native commit
checks time. -/
structure ObservedTransaction (Generation : Type) where
  before : World Generation
  admitted : World Generation
  deriving DecidableEq, Repr

def observeTransaction? {Generation : Type} [DecidableEq Generation]
    (pre : World Generation) (action : Action Generation) :
    Option (ObservedTransaction Generation) :=
  (step? pre action).map fun post => ⟨pre, post⟩

/-- Native OCC: a changed authoritative request observation rejects the commit; wall-clock passage alone does not. Admission before expiry is not a promise the commit finishes before expiry; the pinned native adapter must demonstrate the request point read (a same-value write or arbitrary scan is insufficient). -/
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
