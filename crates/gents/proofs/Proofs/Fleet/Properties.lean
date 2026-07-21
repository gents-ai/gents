import Proofs.Fleet.Executable

namespace FleetState

theorem slotCountFor_split_member
    (s : FleetState) (wid : Nat) (bid : BackendId)
    (hmem : wid ∈ s.activeIds) :
    slotCountFor s bid =
      slotContribution (s.ctx wid) bid +
        ∑ id ∈ s.activeIds.erase wid, slotContribution (s.ctx id) bid := by
  unfold slotCountFor
  simpa using (Finset.add_sum_erase s.activeIds (fun id => slotContribution (s.ctx id) bid) hmem).symm

theorem slotCountFor_update_member
    (s : FleetState) (wid : Nat) (ctx' : RequestContext) (bid : BackendId)
    (hmem : wid ∈ s.activeIds) :
    slotCountFor { s with ctx := Function.update s.ctx wid ctx' } bid =
      slotContribution ctx' bid +
        ∑ id ∈ s.activeIds.erase wid, slotContribution (s.ctx id) bid := by
  classical
  unfold slotCountFor
  rw [← Finset.add_sum_erase s.activeIds
    (fun id => slotContribution ((Function.update s.ctx wid ctx') id) bid) hmem]
  simp [Function.update_self]
  apply Finset.sum_congr rfl
  intro id hid
  have hne : id ≠ wid := Finset.ne_of_mem_erase hid
  simp [Function.update_of_ne hne]

theorem slotCountFor_insert
    (s : FleetState) (wid : Nat) (ctx' : RequestContext) (bid : BackendId)
    (hnot : wid ∉ s.activeIds) :
    slotCountFor
        { activeIds := insert wid s.activeIds
        , ctx := Function.update s.ctx wid ctx'
        , scheduler := s.scheduler
        } bid =
      slotContribution ctx' bid + slotCountFor s bid := by
  classical
  unfold slotCountFor
  rw [Finset.sum_insert hnot]
  simp [hnot, Function.update_self]
  apply Finset.sum_congr rfl
  intro id hid
  have hne : id ≠ wid := by
    intro h_eq
    apply hnot
    simpa [h_eq] using hid
  simp [Function.update_of_ne hne]

theorem slotContribution_other_backend
    {ctx : RequestContext} {bid other : BackendId}
    (h_backend : ctx.backend = other) (h_ne : bid ≠ other) :
    slotContribution ctx bid = 0 := by
  unfold slotContribution
  have h_other : other ≠ bid := h_ne.symm
  simp [h_backend, h_other]

theorem slotContribution_waiting_same_backend
    {ctx : RequestContext} {bid : BackendId}
    (h_backend : ctx.backend = bid) :
    slotContribution { ctx with admission := .waiting } bid = 0 := by
  unfold slotContribution
  simp [h_backend, AdmissionState.holdsSlot]

theorem slotContribution_released_same_backend
    {ctx : RequestContext} {bid : BackendId}
    (h_backend : ctx.backend = bid) :
    slotContribution { ctx with admission := .released } bid = 0 := by
  unfold slotContribution
  simp [h_backend, AdmissionState.holdsSlot]

theorem slotContribution_acquired_same_backend
    {ctx : RequestContext} {bid : BackendId}
    (h_backend : ctx.backend = bid) :
    slotContribution { ctx with admission := .acquired } bid = 1 := by
  unfold slotContribution
  simp [h_backend, AdmissionState.holdsSlot]

theorem slotContribution_executing_same_backend
    {ctx : RequestContext} {bid : BackendId}
    (h_backend : ctx.backend = bid) :
    slotContribution { ctx with admission := .executing } bid = 1 := by
  unfold slotContribution
  simp [h_backend, AdmissionState.holdsSlot]

end FleetState
