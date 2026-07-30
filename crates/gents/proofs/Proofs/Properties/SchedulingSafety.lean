import Proofs.Fleet

open AdmissionState SchedulerState FleetState

theorem capacity_invariant_preserved
    {pre post : FleetState}
    (h_inv : SchedulerState.capacityInvariant pre.scheduler)
    (h_trans : FleetState.Transition pre post) :
    SchedulerState.capacityInvariant post.scheduler := by
  intro bid
  cases h_trans with
  | materialize_scheduled _ _ _ _ _ h_sched =>
    rw [h_sched]
    exact h_inv bid
  | accept_existing _ _ _ _ _ _ h_sched =>
    rw [h_sched]
    exact h_inv bid
  | acquire_slot wid touched h_guard _ _ h_backends h_running =>
    rcases h_guard with ⟨_, _, _, _, _, h_cap⟩
    by_cases h_eq : bid = touched
    · subst bid
      rw [h_backends, h_running, Function.update_self]
      exact Nat.succ_le_of_lt h_cap
    · rw [h_backends, h_running, Function.update_of_ne h_eq]
      exact h_inv bid
  | begin_execution _ _ _ _ h_sched =>
    rw [h_sched]
    exact h_inv bid
  | release_on_terminal _ touched _ h_guard _ _ h_backends h_running =>
    rcases h_guard with ⟨_, _, _, h_pos, _⟩
    by_cases h_eq : bid = touched
    · subst bid
      rw [h_backends, h_running, Function.update_self]
      exact Nat.le_trans (Nat.sub_le _ _) (h_inv touched)
    · rw [h_backends, h_running, Function.update_of_ne h_eq]
      exact h_inv bid

theorem slot_accounting_preserved
    {pre post : FleetState}
    (h_inv : FleetState.slotAccountingInvariant pre)
    (h_trans : FleetState.Transition pre post) :
    FleetState.slotAccountingInvariant post := by
  unfold FleetState.slotAccountingInvariant at h_inv ⊢
  intro bid
  cases h_trans with
  | materialize_scheduled wid touched h_fresh h_ids h_ctx h_sched =>
    let newCtx : RequestContext :=
      { state := .claimed
      , origin := .scheduled
      , backend := touched
      , admission := .waiting
      , deadline := 0
      , claimTime := 0
      , currentTime := 0
      , retryCount := 0
      , maxRetries := 3
      , progressSeq := 0
      , messageSeq := 0
      , isLatest := true
      , persistence := .uncommitted
      }
    have h_post :
        post =
          { activeIds := insert wid pre.activeIds
          , ctx := Function.update pre.ctx wid newCtx
          , scheduler := pre.scheduler
          } :=
      FleetState.ext h_ids h_ctx h_sched
    subst post
    rw [h_inv bid]
    rw [FleetState.slotCountFor_insert pre wid newCtx bid h_fresh]
    have h_zero : FleetState.slotContribution newCtx bid = 0 := by
      unfold FleetState.slotContribution
      by_cases h_backend : touched = bid
      · subst bid
        simp [newCtx, AdmissionState.holdsSlot]
      · simp [newCtx, h_backend, AdmissionState.holdsSlot]
    rw [h_zero, zero_add]
  | accept_existing wid h_fresh _ h_wait h_ids h_ctx h_sched =>
    have h_post :
        post =
          { activeIds := insert wid pre.activeIds
          , ctx := pre.ctx
          , scheduler := pre.scheduler
          } :=
      FleetState.ext h_ids h_ctx h_sched
    subst post
    rw [h_inv bid]
    have h_insert := FleetState.slotCountFor_insert pre wid (pre.ctx wid) bid h_fresh
    have h_count :
        FleetState.slotCountFor
            { activeIds := insert wid pre.activeIds
            , ctx := pre.ctx
            , scheduler := pre.scheduler
            } bid =
          FleetState.slotContribution (pre.ctx wid) bid + FleetState.slotCountFor pre bid := by
      simpa [Function.update_eq_self] using h_insert
    rw [h_count]
    have h_zero : FleetState.slotContribution (pre.ctx wid) bid = 0 := by
      unfold FleetState.slotContribution
      by_cases h_backend : (pre.ctx wid).backend = bid
      · simp [h_backend, h_wait, AdmissionState.holdsSlot]
      · simp [h_backend]
    rw [h_zero, zero_add]
  | acquire_slot wid touched h_guard h_ids h_ctx h_backends h_running =>
    rcases h_guard with ⟨hmem, _, h_wait, h_backend, _, _⟩
    let newCtx : RequestContext := { pre.ctx wid with admission := .acquired }
    have h_scheduler :
        post.scheduler =
          { running := Function.update pre.scheduler.running touched (pre.scheduler.running touched + 1)
          , backends := pre.scheduler.backends
          } :=
      SchedulerState.ext h_running h_backends
    have h_post :
        post =
          { activeIds := pre.activeIds
          , ctx := Function.update pre.ctx wid newCtx
          , scheduler :=
              { running := Function.update pre.scheduler.running touched (pre.scheduler.running touched + 1)
              , backends := pre.scheduler.backends
              }
          } :=
      FleetState.ext h_ids h_ctx h_scheduler
    subst post
    have h_runningField :
        ({ activeIds := pre.activeIds
         , ctx := Function.update pre.ctx wid newCtx
         , scheduler :=
             { running := Function.update pre.scheduler.running touched (pre.scheduler.running touched + 1)
             , backends := pre.scheduler.backends
             }
         } : FleetState).scheduler.running =
          Function.update pre.scheduler.running touched (pre.scheduler.running touched + 1) := rfl
    have h_slotCount :
        FleetState.slotCountFor
            { activeIds := pre.activeIds
            , ctx := Function.update pre.ctx wid newCtx
            , scheduler :=
                { running := Function.update pre.scheduler.running touched (pre.scheduler.running touched + 1)
                , backends := pre.scheduler.backends
                }
            } bid =
          FleetState.slotCountFor
            { activeIds := pre.activeIds
            , ctx := Function.update pre.ctx wid newCtx
            , scheduler := pre.scheduler
            } bid := rfl
    rw [h_runningField, h_slotCount]
    by_cases h_eq : bid = touched
    · subst bid
      rw [Function.update_self]
      rw [h_inv touched]
      rw [FleetState.slotCountFor_update_member pre wid newCtx touched hmem]
      rw [FleetState.slotCountFor_split_member pre wid touched hmem]
      have h_old : FleetState.slotContribution (pre.ctx wid) touched = 0 := by
        unfold FleetState.slotContribution
        simp [h_backend, h_wait, AdmissionState.holdsSlot]
      have h_new : FleetState.slotContribution newCtx touched = 1 := by
        exact FleetState.slotContribution_acquired_same_backend h_backend
      rw [h_old, h_new]
      omega
    · rw [Function.update_of_ne h_eq, h_inv bid]
      rw [FleetState.slotCountFor_update_member pre wid newCtx bid hmem]
      rw [FleetState.slotCountFor_split_member pre wid bid hmem]
      have h_old : FleetState.slotContribution (pre.ctx wid) bid = 0 := by
        exact FleetState.slotContribution_other_backend h_backend h_eq
      have h_backend' : newCtx.backend = touched := by
        simp [newCtx, h_backend]
      have h_new : FleetState.slotContribution newCtx bid = 0 := by
        exact FleetState.slotContribution_other_backend h_backend' h_eq
      rw [h_old, h_new]
  | begin_execution wid h_guard h_ids h_ctx h_sched =>
    rcases h_guard with ⟨hmem, _, h_acq⟩
    let newCtx : RequestContext := { pre.ctx wid with state := .processing, admission := .executing }
    have h_post :
        post =
          { activeIds := pre.activeIds
          , ctx := Function.update pre.ctx wid newCtx
          , scheduler := pre.scheduler
          } :=
      FleetState.ext h_ids h_ctx h_sched
    subst post
    rw [h_inv bid]
    rw [FleetState.slotCountFor_update_member pre wid newCtx bid hmem]
    rw [FleetState.slotCountFor_split_member pre wid bid hmem]
    by_cases h_eq : bid = (pre.ctx wid).backend
    · subst bid
      have h_old : FleetState.slotContribution (pre.ctx wid) (pre.ctx wid).backend = 1 := by
        unfold FleetState.slotContribution
        simp [h_acq, AdmissionState.holdsSlot]
      have h_new : FleetState.slotContribution newCtx (pre.ctx wid).backend = 1 := by
        exact FleetState.slotContribution_executing_same_backend rfl
      rw [h_old, h_new]
    · have h_old : FleetState.slotContribution (pre.ctx wid) bid = 0 := by
        exact FleetState.slotContribution_other_backend rfl h_eq
      have h_backend' : newCtx.backend = (pre.ctx wid).backend := by
        rfl
      have h_new : FleetState.slotContribution newCtx bid = 0 := by
        exact FleetState.slotContribution_other_backend h_backend' h_eq
      rw [h_old, h_new]
  | release_on_terminal wid touched terminal h_guard h_ids h_ctx h_backends h_running =>
    rcases h_guard with ⟨hmem, h_backend, h_slot, _, h_term⟩
    let newCtx : RequestContext := RequestContext.releaseToTerminal (pre.ctx wid) terminal
    have h_scheduler :
        post.scheduler =
          { running := Function.update pre.scheduler.running touched (pre.scheduler.running touched - 1)
          , backends := pre.scheduler.backends
          } :=
      SchedulerState.ext h_running h_backends
    have h_post :
        post =
          { activeIds := pre.activeIds
          , ctx := Function.update pre.ctx wid newCtx
          , scheduler :=
              { running := Function.update pre.scheduler.running touched (pre.scheduler.running touched - 1)
              , backends := pre.scheduler.backends
              }
          } :=
      FleetState.ext h_ids h_ctx h_scheduler
    subst post
    have h_runningField :
        ({ activeIds := pre.activeIds
         , ctx := Function.update pre.ctx wid newCtx
         , scheduler :=
             { running := Function.update pre.scheduler.running touched (pre.scheduler.running touched - 1)
             , backends := pre.scheduler.backends
             }
         } : FleetState).scheduler.running =
          Function.update pre.scheduler.running touched (pre.scheduler.running touched - 1) := rfl
    have h_slotCount :
        FleetState.slotCountFor
            { activeIds := pre.activeIds
            , ctx := Function.update pre.ctx wid newCtx
            , scheduler :=
                { running := Function.update pre.scheduler.running touched (pre.scheduler.running touched - 1)
                , backends := pre.scheduler.backends
                }
            } bid =
          FleetState.slotCountFor
            { activeIds := pre.activeIds
            , ctx := Function.update pre.ctx wid newCtx
            , scheduler := pre.scheduler
            } bid := rfl
    rw [h_runningField, h_slotCount]
    by_cases h_eq : bid = touched
    · subst bid
      rw [Function.update_self]
      rw [h_inv touched]
      rw [FleetState.slotCountFor_update_member pre wid newCtx touched hmem]
      rw [FleetState.slotCountFor_split_member pre wid touched hmem]
      have h_old : FleetState.slotContribution (pre.ctx wid) touched = 1 := by
        unfold FleetState.slotContribution
        simp [h_backend, h_slot]
      have h_new : FleetState.slotContribution newCtx touched = 0 := by
        unfold FleetState.slotContribution
        simp [newCtx, h_backend, RequestContext.releaseToTerminal_released, AdmissionState.holdsSlot]
      rw [h_old, h_new]
      omega
    · rw [Function.update_of_ne h_eq, h_inv bid]
      rw [FleetState.slotCountFor_update_member pre wid newCtx bid hmem]
      rw [FleetState.slotCountFor_split_member pre wid bid hmem]
      have h_old : FleetState.slotContribution (pre.ctx wid) bid = 0 := by
        exact FleetState.slotContribution_other_backend h_backend h_eq
      have h_backend' : newCtx.backend = touched := by
        simpa [newCtx, h_backend] using RequestContext.releaseToTerminal_backend (pre.ctx wid) terminal
      have h_new : FleetState.slotContribution newCtx bid = 0 := by
        exact FleetState.slotContribution_other_backend h_backend' h_eq
      rw [h_old, h_new]

theorem terminal_implies_released
    {r : RequestContext}
    (h_coherent : r.coherent)
    (h_term : isTerminal r.state) :
    r.admission = .released :=
  RequestContext.terminal_implies_released_local h_coherent h_term

theorem unavailable_blocks_acquire
    {pre : FleetState}
    {wid : Nat} {bid : BackendId}
    (h_unavail : (pre.scheduler.backends bid).available = false) :
    ¬FleetState.CanAcquire pre wid bid := by
  intro h_can
  rcases h_can with ⟨_, _, _, _, h_avail, _⟩
  rw [h_unavail] at h_avail
  simp at h_avail
