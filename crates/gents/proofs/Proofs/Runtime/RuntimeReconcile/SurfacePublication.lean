/-! How the runtime's control watcher publishes a reconciled configuration
while an installer in another process holds the plugin store. The watcher
builds a proposal only under the store's shared lease, so a proposal never
mixes plugin records from before and after an install. -/

namespace RuntimeReconcile.SurfacePublication

/-- A publication epoch opens at the first change the watcher has not applied
and keeps that deadline until a proposal publishes without contention. Later
changes, retries and busy observations never move it, so a store held without
end delays a revocation by at most one budget. -/
def epochDeadline (opened : Option Nat) (now budget : Nat) : Nat :=
  opened.getD (now + budget)

inductive Disposition where
  | publish | keepPrevious | publishDemotingBusy
  deriving DecidableEq, Repr

/-- What a reconcile does with the store it observed. Without contention it
publishes the latest configuration. While its epoch is open, contention keeps
the active generation: its agents still run the pins they were admitted with.
Once the epoch has expired the latest configuration publishes anyway, so a
revocation or removal applies, and only the agents whose plugin tools could not
be built for the contention are demoted. -/
def reconcile (busy : Bool) (now deadline : Nat) : Disposition :=
  if !busy then .publish
  else if now < deadline then .keepPrevious
  else .publishDemotingBusy

/-- Only an uncontended publication closes the epoch. -/
def closesEpoch : Disposition → Bool
  | .publish => true
  | _ => false

theorem an_open_epoch_is_never_extended (deadline now budget : Nat) :
    epochDeadline (some deadline) now budget = deadline := rfl

theorem an_uncontended_reconcile_publishes (now deadline : Nat) :
    reconcile false now deadline = .publish := rfl

theorem an_expired_epoch_never_keeps_the_previous_generation (busy : Bool) (now deadline : Nat)
    (h : deadline ≤ now) : reconcile busy now deadline ≠ .keepPrevious := by
  have expired : ¬ now < deadline := Nat.not_lt.mpr h
  cases busy <;> simp [reconcile, expired]

theorem contention_within_the_epoch_keeps_the_previous_generation (now deadline : Nat)
    (h : now < deadline) : reconcile true now deadline = .keepPrevious := by
  simp [reconcile, h]

theorem a_demoting_publication_keeps_the_epoch_open (now deadline : Nat) :
    closesEpoch (reconcile true now deadline) = false := by
  by_cases h : now < deadline <;> simp [reconcile, closesEpoch, h]

end RuntimeReconcile.SurfacePublication
