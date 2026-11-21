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

/-- Why an agent is unavailable when the runtime starts, as startup weighs it. -/
inductive StartupCause where
  | degraded | publicationBusy | invalid
  deriving DecidableEq, Repr

/-- A tool surface that could not be built only because an installer held the
plugin store is transient, like the degraded readiness reasons; any other
unbuilt surface is invalid configuration. -/
def startupCause (degradedReason surfaceUnavailable publicationBusy : Bool) : StartupCause :=
  if degradedReason then .degraded
  else if surfaceUnavailable && publicationBusy then .publicationBusy
  else .invalid

/-- Startup proceeds with a runnable agent, or with none when every
unavailable agent has a transient cause; invalid configuration blocks it.
A node whose agents all wait on a held store starts degraded and recovers
when the store is released. -/
def startupProceeds (runnable : Nat) (causes : List StartupCause) : Bool :=
  decide (0 < runnable) || (!causes.isEmpty && causes.all (· != .invalid))

/-- A reconciled proposal is sent when the configuration changed, or when a
recovery is pending: a slot parked by a held store has an unchanged
configuration, so deduplicating by fingerprint would strand it. -/
def sendProposal (changed recoveryPending : Bool) : Bool := changed || recoveryPending

/-- A slot is rebuilt when its configuration changed or it is parked: a parked
slot runs nothing, so a matching configuration does not make it reusable. -/
def recreateSlot (configurationMatches parked : Bool) : Bool := !configurationMatches || parked

/-- After a failed reconcile the watcher retries while the change is still
settling or a recovery is pending; otherwise it goes idle until the next
event, and a pending recovery would be lost. -/
def retryAfterFailure (settling recoveryPending : Bool) : Bool := settling || recoveryPending

theorem a_store_held_at_startup_does_not_block_it :
    startupProceeds 0 [.publicationBusy, .degraded] = true := by decide

theorem invalid_configuration_still_blocks_startup :
    startupProceeds 0 [.publicationBusy, .invalid] = false := by decide

theorem a_pending_recovery_is_never_deduplicated (changed : Bool) :
    sendProposal changed true = true := by cases changed <;> rfl

theorem a_parked_slot_is_rebuilt (configurationMatches : Bool) :
    recreateSlot configurationMatches true = true := by cases configurationMatches <;> rfl

theorem a_failed_reconcile_keeps_a_pending_recovery (settling : Bool) :
    retryAfterFailure settling true = true := by cases settling <;> rfl

end RuntimeReconcile.SurfacePublication
