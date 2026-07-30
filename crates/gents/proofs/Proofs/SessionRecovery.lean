import Proofs.Request
import Mathlib.Data.Finset.Basic

structure SessionState where
  sessionId : SessionId
  behaviorId : BehaviorId
  requestIds : Finset RequestId
  ctx : RequestId → RequestContext
  latest : RequestId

namespace SessionState

@[ext] theorem ext
    {s t : SessionState}
    (h_sessionId : s.sessionId = t.sessionId)
    (h_behaviorId : s.behaviorId = t.behaviorId)
    (h_requestIds : s.requestIds = t.requestIds)
    (h_ctx : s.ctx = t.ctx)
    (h_latest : s.latest = t.latest) :
    s = t := by
  cases s
  cases t
  cases h_sessionId
  cases h_behaviorId
  cases h_requestIds
  cases h_ctx
  cases h_latest
  rfl

def latestFlagInvariant (s : SessionState) : Prop :=
  s.latest ∈ s.requestIds ∧
    (s.ctx s.latest).isLatest = true ∧
    ∀ rid, rid ∈ s.requestIds → rid ≠ s.latest → (s.ctx rid).isLatest = false

def historicalContext (ctx : RequestContext) : RequestContext :=
  { ctx with isLatest := false }

def reissuedContext (ctx : RequestContext) : RequestContext :=
  { state := .pending
  , origin := ctx.origin
  , backend := ctx.backend
  , admission := .released
  , deadline := ctx.currentTime + 1
  , claimTime := ctx.currentTime
  , currentTime := ctx.currentTime
  , retryCount := ctx.retryCount + 1
  , maxRetries := ctx.maxRetries
  , progressSeq := 0
  , messageSeq := 0
  , isLatest := true
  , persistence := .uncommitted
  }

def CanReissue (pre : SessionState) (failedId newId : RequestId) : Prop :=
  failedId = pre.latest ∧
    failedId ∈ pre.requestIds ∧
    newId ∉ pre.requestIds ∧
    (pre.ctx failedId).state = .failed ∧
    (pre.ctx failedId).admission = .released ∧
    (pre.ctx failedId).retryCount < (pre.ctx failedId).maxRetries ∧
    ¬ (pre.ctx failedId).deadlineExceeded ∧
    (pre.ctx failedId).isLatest = true

instance (pre : SessionState) (failedId newId : RequestId) :
    Decidable (CanReissue pre failedId newId) := by
  unfold CanReissue
  infer_instance

inductive Transition : SessionState → SessionState → Prop where
  | reissue_failed {pre post : SessionState} (failedId newId : RequestId) :
      CanReissue pre failedId newId →
      post.sessionId = pre.sessionId →
      post.behaviorId = pre.behaviorId →
      post.requestIds = insert newId pre.requestIds →
      post.latest = newId →
      post.ctx =
        Function.update
          (Function.update pre.ctx failedId (historicalContext (pre.ctx failedId)))
          newId
          (reissuedContext (pre.ctx failedId)) →
      Transition pre post

inductive Action where
  | reissueFailed (failedId newId : RequestId)
  deriving DecidableEq, Repr

def step? (pre : SessionState) : Action → Option SessionState
  | .reissueFailed failedId newId =>
      if _h_reissue : CanReissue pre failedId newId then
        some
          { sessionId := pre.sessionId
          , behaviorId := pre.behaviorId
          , requestIds := insert newId pre.requestIds
          , latest := newId
          , ctx :=
              Function.update
                (Function.update pre.ctx failedId (historicalContext (pre.ctx failedId)))
                newId
                (reissuedContext (pre.ctx failedId))
          }
      else
        none

inductive Trace : SessionState → SessionState → Prop where
  | refl {s : SessionState} : Trace s s
  | step {s₁ s₂ s₃ : SessionState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

def replay? : SessionState → List Action → Option SessionState
  | s, [] => some s
  | s, action :: rest =>
      match step? s action with
      | some s' => replay? s' rest
      | none => none

theorem step_sound
    {pre post : SessionState}
    {action : Action}
    (h_step : step? pre action = some post) :
    Transition pre post := by
  cases action with
  | reissueFailed failedId newId =>
      simp [step?] at h_step
      rcases h_step with ⟨h_reissue, h_post⟩
      subst post
      exact Transition.reissue_failed failedId newId h_reissue rfl rfl rfl rfl rfl

theorem transition_complete
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    ∃ action : Action, step? pre action = some post := by
  cases h_trans with
  | reissue_failed failedId newId h_reissue h_session h_behavior h_requestIds h_latest h_ctx =>
      have h_post :
          { sessionId := pre.sessionId
          , behaviorId := pre.behaviorId
          , requestIds := insert newId pre.requestIds
          , latest := newId
          , ctx :=
              Function.update
                (Function.update pre.ctx failedId (historicalContext (pre.ctx failedId)))
                newId
                (reissuedContext (pre.ctx failedId))
          } = post := by
        apply ext
        · exact h_session.symm
        · exact h_behavior.symm
        · exact h_requestIds.symm
        · exact h_ctx.symm
        · exact h_latest.symm
      exact ⟨.reissueFailed failedId newId, by simp [step?, h_reissue, h_post]⟩

theorem replay_sound
    {pre post : SessionState}
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
    {pre post : SessionState}
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

theorem reissuedContext_pending
    (ctx : RequestContext) :
    (reissuedContext ctx).state = .pending := by
  rfl

theorem reissuedContext_released
    (ctx : RequestContext) :
    (reissuedContext ctx).admission = .released := by
  rfl

theorem reissuedContext_origin
    (ctx : RequestContext) :
    (reissuedContext ctx).origin = ctx.origin := by
  rfl

theorem reissuedContext_backend
    (ctx : RequestContext) :
    (reissuedContext ctx).backend = ctx.backend := by
  rfl

theorem reissuedContext_retryCount
    (ctx : RequestContext) :
    (reissuedContext ctx).retryCount = ctx.retryCount + 1 := by
  rfl

theorem reissuedContext_retryBound
    {ctx : RequestContext}
    (h_budget : ctx.retryCount < ctx.maxRetries) :
    (reissuedContext ctx).retryCount ≤ (reissuedContext ctx).maxRetries := by
  simpa [reissuedContext] using Nat.succ_le_of_lt h_budget

theorem reissuedContext_deadline_open
    (ctx : RequestContext) :
    ¬ (reissuedContext ctx).deadlineExceeded := by
  simp [RequestContext.deadlineExceeded, reissuedContext]

theorem reissuedContext_coherent
    (ctx : RequestContext) :
    (reissuedContext ctx).coherent := by
  simp [RequestContext.coherent, RequestContext.coherentStateAdmission, reissuedContext]

theorem reissue_preserves_session
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    post.sessionId = pre.sessionId := by
  cases h_trans with
  | reissue_failed _ _ _ h_session _ _ _ _ => exact h_session

theorem reissue_preserves_behavior
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    post.behaviorId = pre.behaviorId := by
  cases h_trans with
  | reissue_failed _ _ _ _ h_behavior _ _ _ => exact h_behavior

theorem reissue_latest_in_requestIds
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    post.latest ∈ post.requestIds := by
  cases h_trans with
  | reissue_failed _ _ _ _ _ h_requestIds h_latest _ =>
      rw [h_latest, h_requestIds]
      exact Finset.mem_insert_self _ _

theorem reissue_latest_pending
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    (post.ctx post.latest).state = .pending := by
  cases h_trans with
  | reissue_failed _ _ h_can _ _ _ h_latest h_ctx =>
      rw [h_latest, h_ctx]
      simp [reissuedContext]

theorem reissue_latest_released
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    (post.ctx post.latest).admission = .released := by
  cases h_trans with
  | reissue_failed _ _ h_can _ _ _ h_latest h_ctx =>
      rw [h_latest, h_ctx]
      simp [reissuedContext]

theorem reissue_latest_origin_preserved
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    (post.ctx post.latest).origin = (pre.ctx pre.latest).origin := by
  cases h_trans with
  | reissue_failed _ _ h_can _ _ _ h_latest h_ctx =>
      rcases h_can with ⟨h_failed_latest, _, _, _, _, _, _, _⟩
      rw [h_latest, h_ctx, h_failed_latest]
      simp [reissuedContext]

theorem reissue_latest_backend_preserved
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    (post.ctx post.latest).backend = (pre.ctx pre.latest).backend := by
  cases h_trans with
  | reissue_failed _ _ h_can _ _ _ h_latest h_ctx =>
      rcases h_can with ⟨h_failed_latest, _, _, _, _, _, _, _⟩
      rw [h_latest, h_ctx, h_failed_latest]
      simp [reissuedContext]

theorem reissue_latest_retryCount_succ
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    (post.ctx post.latest).retryCount = (pre.ctx pre.latest).retryCount + 1 := by
  cases h_trans with
  | reissue_failed _ _ h_can _ _ _ h_latest h_ctx =>
      rcases h_can with ⟨h_failed_latest, _, _, _, _, _, _, _⟩
      rw [h_latest, h_ctx, h_failed_latest]
      simp [reissuedContext]

theorem reissue_latest_retryBound
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    (post.ctx post.latest).retryCount ≤ (post.ctx post.latest).maxRetries := by
  cases h_trans with
  | reissue_failed _ _ h_can _ _ _ h_latest h_ctx =>
      rcases h_can with ⟨h_failed_latest, _, _, _, _, h_budget, _, _⟩
      have h_budget_latest : (pre.ctx pre.latest).retryCount < (pre.ctx pre.latest).maxRetries := by
        simpa [← h_failed_latest] using h_budget
      rw [h_latest, h_ctx, h_failed_latest]
      simpa [reissuedContext] using reissuedContext_retryBound h_budget_latest

theorem reissue_source_deadline_open
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    ¬ (pre.ctx pre.latest).deadlineExceeded := by
  cases h_trans with
  | reissue_failed _ _ h_can _ _ _ _ _ =>
      rcases h_can with ⟨h_failed_latest, _, _, _, _, _, h_deadline, _⟩
      rw [h_failed_latest] at h_deadline
      exact h_deadline

theorem reissue_latest_deadline_open
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    ¬ (post.ctx post.latest).deadlineExceeded := by
  cases h_trans with
  | reissue_failed failedId newId _ _ _ _ h_latest h_ctx =>
      rw [h_latest, h_ctx]
      simpa [Function.update_self] using reissuedContext_deadline_open (pre.ctx failedId)

theorem reissue_demotes_previous_latest
    {pre post : SessionState}
    (h_trans : Transition pre post) :
    (post.ctx pre.latest).isLatest = false := by
  cases h_trans with
  | reissue_failed failedId newId h_can _ _ _ _ h_ctx =>
      rcases h_can with ⟨h_failed_latest, h_failed_mem, h_new, _, _, _, _, _⟩
      have h_distinct : newId ≠ pre.latest := by
        intro h_eq
        have h_latest_mem : pre.latest ∈ pre.requestIds := by
          simpa [← h_failed_latest] using h_failed_mem
        have h_new_mem : newId ∈ pre.requestIds := by
          simpa [h_eq] using h_latest_mem
        exact h_new h_new_mem
      rw [h_ctx, h_failed_latest]
      rw [Function.update_of_ne h_distinct.symm, Function.update_self]
      simp [historicalContext]

theorem reissue_preserves_latestFlagInvariant
    {pre post : SessionState}
    (h_pre : pre.latestFlagInvariant)
    (h_trans : Transition pre post) :
    post.latestFlagInvariant := by
  cases h_trans with
  | reissue_failed failedId newId h_can _ _ h_requestIds h_latest h_ctx =>
      rcases h_pre with ⟨h_pre_latest_mem, h_pre_latest_flag, h_pre_others⟩
      rcases h_can with ⟨h_failed_latest, h_failed_mem, h_new, _, _, _, _, _⟩
      constructor
      · rw [h_latest, h_requestIds]
        exact Finset.mem_insert_self _ _
      constructor
      · rw [h_latest, h_ctx]
        simp [reissuedContext]
      · intro rid h_rid_mem h_rid_ne_latest
        rw [h_ctx]
        rw [h_latest] at h_rid_ne_latest
        by_cases h_rid_new : rid = newId
        · exact False.elim (h_rid_ne_latest h_rid_new)
        · by_cases h_rid_failed : rid = failedId
          · subst h_rid_failed
            have h_failed_ne_new : rid ≠ newId := by
              intro h_eq
              rw [h_eq] at h_failed_mem
              exact h_new h_failed_mem
            rw [Function.update_of_ne h_failed_ne_new, Function.update_self]
            simp [historicalContext]
          · have h_rid_mem_pre : rid ∈ pre.requestIds := by
              rw [h_requestIds] at h_rid_mem
              exact Finset.mem_of_mem_insert_of_ne h_rid_mem h_rid_new
            have h_rid_ne_pre_latest : rid ≠ pre.latest := by
              intro h_eq
              apply h_rid_failed
              rw [h_failed_latest]
              exact h_eq
            have h_old_flag : (pre.ctx rid).isLatest = false :=
              h_pre_others rid h_rid_mem_pre h_rid_ne_pre_latest
            have h_rid_ne_failed : rid ≠ failedId := h_rid_failed
            simp [Function.update_of_ne h_rid_new, Function.update_of_ne h_rid_ne_failed, h_old_flag]

end SessionState
