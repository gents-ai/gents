import Proofs.Fleet
import Proofs.InferenceCall.SlotAccounting

namespace FleetState

theorem slotAccountingInvariant_reconstructs_running
    {s : FleetState}
    (h : s.slotAccountingInvariant)
    (bid : BackendId) :
    s.scheduler.running bid = s.slotCountFor bid :=
  h bid

end FleetState

namespace InferenceCall

theorem scheduler_running_reconstructed_from_inference_calls
    {callIds : Finset Nat}
    {row : Nat → InferenceCall}
    {scheduler : SchedulerState}
    (h_reconstruct : ReconstructsSchedulerRunning callIds row scheduler)
    (bid : BackendId) :
    scheduler.running bid = reconstructedSlotCount callIds row bid :=
  h_reconstruct bid

theorem reconstructed_counts_respect_scheduler_capacity
    {callIds : Finset Nat}
    {row : Nat → InferenceCall}
    {scheduler : SchedulerState}
    (h_reconstruct : ReconstructsSchedulerRunning callIds row scheduler)
    (h_capacity : SchedulerState.capacityInvariant scheduler)
    (bid : BackendId) :
    reconstructedSlotCount callIds row bid ≤ (scheduler.backends bid).max_concurrent :=
  reconstructedSlotCount_bounded_by_max_concurrent h_reconstruct h_capacity bid

end InferenceCall
