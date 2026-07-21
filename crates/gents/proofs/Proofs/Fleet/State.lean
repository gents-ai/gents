import Proofs.Request
import Mathlib.Algebra.BigOperators.Group.Finset.Basic
import Mathlib.Data.Finset.Card

/-!
# Fleet-Level Scheduling Model

Finite active-work model tying aggregate backend counts to concrete executions.
-/

open AdmissionState SchedulerState
open scoped BigOperators

/-- Fleet state: a finite set of active work identifiers plus their contexts. -/
structure FleetState where
  activeIds : Finset Nat
  ctx : Nat → RequestContext
  scheduler : SchedulerState

namespace FleetState

/-- Extensionality for fleet states. -/
@[ext] theorem ext
    {s t : FleetState}
    (h_activeIds : s.activeIds = t.activeIds)
    (h_ctx : s.ctx = t.ctx)
    (h_scheduler : s.scheduler = t.scheduler) :
    s = t := by
  cases s
  cases t
  cases h_activeIds
  cases h_ctx
  cases h_scheduler
  rfl

/-- Lookup the request context for an active work identifier. -/
def lookup (s : FleetState) (wid : Nat) : RequestContext :=
  s.ctx wid

/-- One unit of slot-accounting contribution for a backend. -/
def slotContribution (ctx : RequestContext) (bid : BackendId) : Nat :=
  if ctx.backend = bid ∧ holdsSlot ctx.admission then 1 else 0

/-- Count the work items in `activeIds` currently holding a slot on `bid`. -/
def slotCountFor (s : FleetState) (bid : BackendId) : Nat :=
  ∑ wid ∈ s.activeIds, slotContribution (s.ctx wid) bid

/-- Aggregate counts must equal the number of slot-holding executions. -/
def slotAccountingInvariant (s : FleetState) : Prop :=
  ∀ bid : BackendId, s.scheduler.running bid = slotCountFor s bid

/-- A work item is eligible to acquire backend capacity. -/
def CanAcquire (pre : FleetState) (wid : Nat) (bid : BackendId) : Prop :=
  wid ∈ pre.activeIds ∧
    (pre.ctx wid).state = .claimed ∧
    (pre.ctx wid).admission = .waiting ∧
    (pre.ctx wid).backend = bid ∧
    (pre.scheduler.backends bid).available = true ∧
    pre.scheduler.running bid < (pre.scheduler.backends bid).max_concurrent

instance (pre : FleetState) (wid : Nat) (bid : BackendId) :
    Decidable (CanAcquire pre wid bid) := by
  unfold CanAcquire
  infer_instance

/-- A work item is ready to transition from acquired to executing. -/
def CanBegin (pre : FleetState) (wid : Nat) : Prop :=
  wid ∈ pre.activeIds ∧
    (pre.ctx wid).state = .claimed ∧
    (pre.ctx wid).admission = .acquired

instance (pre : FleetState) (wid : Nat) :
    Decidable (CanBegin pre wid) := by
  unfold CanBegin
  infer_instance

/-- A slot-holding work item may release into a terminal request state. -/
def CanRelease (pre : FleetState) (wid : Nat) (bid : BackendId) (terminal : RequestState) : Prop :=
  wid ∈ pre.activeIds ∧
    (pre.ctx wid).backend = bid ∧
    holdsSlot (pre.ctx wid).admission ∧
    pre.scheduler.running bid > 0 ∧
    isTerminal terminal

instance (pre : FleetState) (wid : Nat) (bid : BackendId) (terminal : RequestState) :
    Decidable (CanRelease pre wid bid terminal) := by
  unfold CanRelease
  infer_instance

end FleetState
