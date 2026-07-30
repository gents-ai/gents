import Proofs.Fleet
import Proofs.Properties.Liveness

open FleetState AdmissionState

theorem acquire_when_capacity_available
    {pre : FleetState} {wid : Nat} {bid : BackendId}
    (h_can : FleetState.CanAcquire pre wid bid) :
    ∃ post, FleetState.Transition pre post ∧ (post.ctx wid).admission = .acquired := by
  let post : FleetState :=
    { activeIds := pre.activeIds
    , ctx := Function.update pre.ctx wid { pre.ctx wid with admission := .acquired }
    , scheduler :=
      { running := Function.update pre.scheduler.running bid (pre.scheduler.running bid + 1)
      , backends := pre.scheduler.backends
      }
    }
  refine ⟨post, ?_, ?_⟩
  · exact FleetState.Transition.acquire_slot wid bid h_can rfl rfl rfl rfl
  · simp [post]

theorem accepted_work_eventually_releases
    {pre : FleetState} {wid : Nat} {bid : BackendId}
    (hmem : wid ∈ pre.activeIds)
    (h_state : (pre.ctx wid).state = .claimed)
    (h_acquired : (pre.ctx wid).admission = .acquired)
    (h_backend : (pre.ctx wid).backend = bid)
    (h_running : pre.scheduler.running bid > 0) :
    ∃ post, FleetState.Trace pre post ∧
      isTerminal (post.lookup wid).state ∧
      (post.lookup wid).admission = .released := by
  let mid : FleetState :=
    { activeIds := pre.activeIds
    , ctx := Function.update pre.ctx wid { pre.ctx wid with state := .processing, admission := .executing }
    , scheduler := pre.scheduler
    }
  have h_begin_guard : FleetState.CanBegin pre wid := ⟨hmem, h_state, h_acquired⟩
  have h_begin : FleetState.Transition pre mid :=
    FleetState.Transition.begin_execution wid h_begin_guard rfl rfl rfl
  let post : FleetState :=
    { activeIds := mid.activeIds
    , ctx := Function.update mid.ctx wid (RequestContext.releaseToTerminal (mid.ctx wid) .completed)
    , scheduler :=
      { running := Function.update mid.scheduler.running bid (mid.scheduler.running bid - 1)
      , backends := mid.scheduler.backends
      }
    }
  have h_release_guard : FleetState.CanRelease mid wid bid .completed := by
    refine ⟨?_, ?_, ?_, ?_, Or.inl rfl⟩
    · simpa [mid]
    · simp [mid, h_backend]
    · simp [mid, AdmissionState.holdsSlot]
    · simpa [mid] using h_running
  have h_release : FleetState.Transition mid post :=
    FleetState.Transition.release_on_terminal wid bid .completed h_release_guard rfl rfl rfl rfl
  refine ⟨post, FleetState.Trace.step h_begin (FleetState.Trace.step h_release FleetState.Trace.refl), ?_, ?_⟩
  · have h_terminal :
        isTerminal (RequestContext.releaseToTerminal (mid.ctx wid) .completed).state := by
      rw [RequestContext.releaseToTerminal_state (Or.inl rfl)]
      exact Or.inl rfl
    simpa [FleetState.lookup, post] using h_terminal
  · simp [FleetState.lookup, post, RequestContext.releaseToTerminal_released]
