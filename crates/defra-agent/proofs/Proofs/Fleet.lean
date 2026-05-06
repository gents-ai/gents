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

/-- Fleet transitions coupling a single execution with aggregate scheduler state. -/
inductive Transition : FleetState → FleetState → Prop where
  | materialize_scheduled {pre post : FleetState} (wid : Nat) (bid : BackendId) :
      wid ∉ pre.activeIds →
      post.activeIds = insert wid pre.activeIds →
      post.ctx = Function.update pre.ctx wid
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
        } →
      post.scheduler = pre.scheduler →
      Transition pre post
  | accept_existing {pre post : FleetState} (wid : Nat) :
      wid ∉ pre.activeIds →
      (pre.ctx wid).state = .claimed →
      (pre.ctx wid).admission = .waiting →
      post.activeIds = insert wid pre.activeIds →
      post.ctx = pre.ctx →
      post.scheduler = pre.scheduler →
      Transition pre post
  | acquire_slot {pre post : FleetState} (wid : Nat) (bid : BackendId) :
      CanAcquire pre wid bid →
      post.activeIds = pre.activeIds →
      post.ctx = Function.update pre.ctx wid { pre.ctx wid with admission := .acquired } →
      post.scheduler.backends = pre.scheduler.backends →
      post.scheduler.running =
        Function.update pre.scheduler.running bid (pre.scheduler.running bid + 1) →
      Transition pre post
  | begin_execution {pre post : FleetState} (wid : Nat) :
      CanBegin pre wid →
      post.activeIds = pre.activeIds →
      post.ctx =
        Function.update pre.ctx wid { pre.ctx wid with state := .processing, admission := .executing } →
      post.scheduler = pre.scheduler →
      Transition pre post
  | release_on_terminal {pre post : FleetState} (wid : Nat) (bid : BackendId)
      (terminal : RequestState) :
      CanRelease pre wid bid terminal →
      post.activeIds = pre.activeIds →
      post.ctx = Function.update pre.ctx wid (RequestContext.releaseToTerminal (pre.ctx wid) terminal) →
      post.scheduler.backends = pre.scheduler.backends →
      post.scheduler.running =
        Function.update pre.scheduler.running bid (pre.scheduler.running bid - 1) →
      Transition pre post

/-- Executable fleet actions mirroring `Transition`. -/
inductive Action where
  | materializeScheduled (wid : Nat) (bid : BackendId)
  | acceptExisting (wid : Nat)
  | acquireSlot (wid : Nat) (bid : BackendId)
  | beginExecution (wid : Nat)
  | releaseOnTerminal (wid : Nat) (bid : BackendId) (terminal : RequestState)
  deriving DecidableEq, Repr

/-- Executable transition function for the fleet layer. -/
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

/-- A trace is a sequence of fleet transitions. -/
inductive Trace : FleetState → FleetState → Prop where
  | refl {s : FleetState} : Trace s s
  | step {s₁ s₂ s₃ : FleetState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

/-- Replay a finite action list through the executable fleet semantics. -/
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
