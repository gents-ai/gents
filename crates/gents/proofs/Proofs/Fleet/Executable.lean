import Proofs.Fleet.Transition

namespace FleetState

inductive Action where
  | materializeScheduled (wid : Nat) (bid : BackendId)
  | acceptExisting (wid : Nat)
  | acquireSlot (wid : Nat) (bid : BackendId)
  | beginExecution (wid : Nat)
  | releaseOnTerminal (wid : Nat) (bid : BackendId) (terminal : RequestState)
  deriving DecidableEq, Repr

def step? (pre : FleetState) : Action → Option FleetState
  | .materializeScheduled wid bid =>
      if _h_not : wid ∉ pre.activeIds then
        some
          { activeIds := insert wid pre.activeIds
          , ctx := Function.update pre.ctx wid
              { state := .claimed
              , origin := .scheduled
              , backend := bid
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
          , scheduler := pre.scheduler
          }
      else
        none
  | .acceptExisting wid =>
      if _h_accept :
          wid ∉ pre.activeIds ∧
          (pre.ctx wid).state = .claimed ∧
          (pre.ctx wid).admission = .waiting then
        some
          { activeIds := insert wid pre.activeIds
          , ctx := pre.ctx
          , scheduler := pre.scheduler
          }
      else
        none
  | .acquireSlot wid bid =>
      if _h_acquire : CanAcquire pre wid bid then
        some
          { activeIds := pre.activeIds
          , ctx := Function.update pre.ctx wid { pre.ctx wid with admission := .acquired }
          , scheduler :=
              { backends := pre.scheduler.backends
              , running := Function.update pre.scheduler.running bid (pre.scheduler.running bid + 1)
              }
          }
      else
        none
  | .beginExecution wid =>
      if _h_begin : CanBegin pre wid then
        some
          { activeIds := pre.activeIds
          , ctx :=
              Function.update pre.ctx wid
                { pre.ctx wid with state := .processing, admission := .executing }
          , scheduler := pre.scheduler
          }
      else
        none
  | .releaseOnTerminal wid bid terminal =>
      if _h_release : CanRelease pre wid bid terminal then
        some
          { activeIds := pre.activeIds
          , ctx := Function.update pre.ctx wid (RequestContext.releaseToTerminal (pre.ctx wid) terminal)
          , scheduler :=
              { backends := pre.scheduler.backends
              , running := Function.update pre.scheduler.running bid (pre.scheduler.running bid - 1)
              }
          }
      else
        none

def replay? : FleetState → List Action → Option FleetState
  | s, [] => some s
  | s, action :: rest =>
      match step? s action with
      | some s' => replay? s' rest
      | none => none

theorem step_sound
    {pre post : FleetState}
    {action : Action}
    (h_step : step? pre action = some post) :
    Transition pre post := by
  cases action with
  | materializeScheduled wid bid =>
      simp [step?] at h_step
      rcases h_step with ⟨h_not, h_post⟩
      subst post
      exact Transition.materialize_scheduled wid bid h_not rfl rfl rfl
  | acceptExisting wid =>
      simp [step?] at h_step
      rcases h_step with ⟨h_accept, h_post⟩
      rcases h_accept with ⟨h_not, h_state, h_admission⟩
      subst post
      exact Transition.accept_existing wid h_not h_state h_admission rfl rfl rfl
  | acquireSlot wid bid =>
      simp [step?] at h_step
      rcases h_step with ⟨h_acquire, h_post⟩
      subst post
      exact Transition.acquire_slot wid bid h_acquire rfl rfl rfl rfl
  | beginExecution wid =>
      simp [step?] at h_step
      rcases h_step with ⟨h_begin, h_post⟩
      subst post
      exact Transition.begin_execution wid h_begin rfl rfl rfl
  | releaseOnTerminal wid bid terminal =>
      simp [step?] at h_step
      rcases h_step with ⟨h_release, h_post⟩
      subst post
      exact Transition.release_on_terminal wid bid terminal h_release rfl rfl rfl rfl

theorem transition_complete
    {pre post : FleetState}
    (h_trans : Transition pre post) :
    ∃ action : Action, step? pre action = some post := by
  cases h_trans with
  | materialize_scheduled wid bid h_not h_activeIds h_ctx h_scheduler =>
      have h_post :
          { activeIds := insert wid pre.activeIds
          , ctx := Function.update pre.ctx wid
              { state := .claimed
              , origin := .scheduled
              , backend := bid
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
          , scheduler := pre.scheduler
          } = post := by
        apply ext
        · exact h_activeIds.symm
        · exact h_ctx.symm
        · exact h_scheduler.symm
      exact ⟨.materializeScheduled wid bid, by simp [step?, h_not, h_post]⟩
  | accept_existing wid h_not h_state h_admission h_activeIds h_ctx h_scheduler =>
      have h_post :
          { activeIds := insert wid pre.activeIds
          , ctx := pre.ctx
          , scheduler := pre.scheduler
          } = post := by
        apply ext
        · exact h_activeIds.symm
        · exact h_ctx.symm
        · exact h_scheduler.symm
      exact ⟨.acceptExisting wid, by simp [step?, h_not, h_state, h_admission, h_post]⟩
  | acquire_slot wid bid h_acquire h_activeIds h_ctx h_backends h_running =>
      have h_scheduler :
          { backends := pre.scheduler.backends
          , running := Function.update pre.scheduler.running bid (pre.scheduler.running bid + 1)
          } = post.scheduler := by
        apply SchedulerState.ext
        · exact h_running.symm
        · exact h_backends.symm
      have h_post :
          { activeIds := pre.activeIds
          , ctx := Function.update pre.ctx wid { pre.ctx wid with admission := .acquired }
          , scheduler :=
              { backends := pre.scheduler.backends
              , running := Function.update pre.scheduler.running bid (pre.scheduler.running bid + 1)
              }
          } = post := by
        apply ext
        · exact h_activeIds.symm
        · exact h_ctx.symm
        · exact h_scheduler
      exact ⟨.acquireSlot wid bid, by simp [step?, h_acquire, h_post]⟩
  | begin_execution wid h_begin h_activeIds h_ctx h_scheduler =>
      have h_post :
          { activeIds := pre.activeIds
          , ctx :=
              Function.update pre.ctx wid
                { pre.ctx wid with state := .processing, admission := .executing }
          , scheduler := pre.scheduler
          } = post := by
        apply ext
        · exact h_activeIds.symm
        · exact h_ctx.symm
        · exact h_scheduler.symm
      exact ⟨.beginExecution wid, by simp [step?, h_begin, h_post]⟩
  | release_on_terminal wid bid terminal h_release h_activeIds h_ctx h_backends h_running =>
      have h_scheduler :
          { backends := pre.scheduler.backends
          , running := Function.update pre.scheduler.running bid (pre.scheduler.running bid - 1)
          } = post.scheduler := by
        apply SchedulerState.ext
        · exact h_running.symm
        · exact h_backends.symm
      have h_post :
          { activeIds := pre.activeIds
          , ctx := Function.update pre.ctx wid (RequestContext.releaseToTerminal (pre.ctx wid) terminal)
          , scheduler :=
              { backends := pre.scheduler.backends
              , running := Function.update pre.scheduler.running bid (pre.scheduler.running bid - 1)
              }
          } = post := by
        apply ext
        · exact h_activeIds.symm
        · exact h_ctx.symm
        · exact h_scheduler
      exact ⟨.releaseOnTerminal wid bid terminal, by simp [step?, h_release, h_post]⟩

theorem replay_sound
    {pre post : FleetState}
    {actions : List Action}
    (h_replay : replay? pre actions = some post) :
    Trace pre post := by
  induction actions generalizing pre with
  | nil =>
      simp [replay?] at h_replay
      subst h_replay
      exact Trace.refl
  | cons action rest ih =>
      simp [replay?] at h_replay
      rcases h_step : step? pre action with (_ | next)
      · simp [h_step] at h_replay
      · simp [h_step] at h_replay
        have h_trans : Transition pre next := step_sound h_step
        exact Trace.step h_trans (ih h_replay)

theorem trace_complete
    {pre post : FleetState}
    (h_trace : Trace pre post) :
    ∃ actions : List Action, replay? pre actions = some post := by
  induction h_trace with
  | refl =>
      exact ⟨[], rfl⟩
  | step h_trans h_trace ih =>
      rcases transition_complete h_trans with ⟨action, h_action⟩
      rcases ih with ⟨actions, h_actions⟩
      refine ⟨action :: actions, ?_⟩
      simp [replay?, h_action, h_actions]

end FleetState
