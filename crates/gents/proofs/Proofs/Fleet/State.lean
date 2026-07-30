import Proofs.Request
import Mathlib.Algebra.BigOperators.Group.Finset.Basic
import Mathlib.Data.Finset.Card

open AdmissionState SchedulerState
open scoped BigOperators

structure FleetState where
  activeIds : Finset Nat
  ctx : Nat → RequestContext
  scheduler : SchedulerState

namespace FleetState

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

def lookup (s : FleetState) (wid : Nat) : RequestContext :=
  s.ctx wid

def slotContribution (ctx : RequestContext) (bid : BackendId) : Nat :=
  if ctx.backend = bid ∧ holdsSlot ctx.admission then 1 else 0

def slotCountFor (s : FleetState) (bid : BackendId) : Nat :=
  ∑ wid ∈ s.activeIds, slotContribution (s.ctx wid) bid

def slotAccountingInvariant (s : FleetState) : Prop :=
  ∀ bid : BackendId, s.scheduler.running bid = slotCountFor s bid

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

def CanBegin (pre : FleetState) (wid : Nat) : Prop :=
  wid ∈ pre.activeIds ∧
    (pre.ctx wid).state = .claimed ∧
    (pre.ctx wid).admission = .acquired

instance (pre : FleetState) (wid : Nat) :
    Decidable (CanBegin pre wid) := by
  unfold CanBegin
  infer_instance

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
