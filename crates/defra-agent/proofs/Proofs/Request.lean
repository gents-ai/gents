import Proofs.Basic
import Proofs.Persistence
import Proofs.Scheduling

/-!
# Layer 2: Request Lifecycle

The core state machine. Models a single request from submission through
terminal state, now refined with backend binding and admission state.
-/

/-- The 9 states of the request lifecycle. -/
inductive RequestState where
  | pending
  | claimed
  | processing
  | inputRequired
  | completed
  | failed
  | superseded
  | dead
  | interrupted
  deriving DecidableEq, Repr

namespace RequestState

instance : HasTerminal RequestState where
  isTerminal s :=
    s = .completed ∨ s = .failed ∨ s = .superseded ∨ s = .dead ∨ s = .interrupted
  isTerminal_dec s :=
    match s with
    | .completed => isTrue (Or.inl rfl)
    | .failed => isTrue (Or.inr (Or.inl rfl))
    | .superseded => isTrue (Or.inr (Or.inr (Or.inl rfl)))
    | .dead => isTrue (Or.inr (Or.inr (Or.inr (Or.inl rfl))))
    | .interrupted => isTrue (Or.inr (Or.inr (Or.inr (Or.inr rfl))))
    | .pending => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => cases h with
          | inl h => exact absurd h (by decide)
          | inr h => cases h with
            | inl h => exact absurd h (by decide)
            | inr h => cases h with
              | inl h => exact absurd h (by decide)
              | inr h => exact absurd h (by decide))
    | .claimed => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => cases h with
          | inl h => exact absurd h (by decide)
          | inr h => cases h with
            | inl h => exact absurd h (by decide)
            | inr h => cases h with
              | inl h => exact absurd h (by decide)
              | inr h => exact absurd h (by decide))
    | .processing => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => cases h with
          | inl h => exact absurd h (by decide)
          | inr h => cases h with
            | inl h => exact absurd h (by decide)
            | inr h => cases h with
              | inl h => exact absurd h (by decide)
              | inr h => exact absurd h (by decide))
    | .inputRequired => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => cases h with
          | inl h => exact absurd h (by decide)
          | inr h => cases h with
            | inl h => exact absurd h (by decide)
            | inr h => cases h with
              | inl h => exact absurd h (by decide)
              | inr h => exact absurd h (by decide))

end RequestState

/-- Mutable per-request context that transitions carry along. -/
structure RequestContext where
  state        : RequestState
  origin       : ExecutionOrigin
  backend      : BackendId
  admission    : AdmissionState
  deadline     : Time
  claimTime    : Time
  currentTime  : Time
  retryCount   : Nat
  maxRetries   : Nat
  progressSeq  : Nat
  messageSeq   : Nat
  isLatest     : Bool
  persistence  : PersistenceState
  /-- Submitter-set interrupt timestamp; runtime-read-only. `none` means no interrupt requested. -/
  interruptRequestedAt : Option Time := none
  /-- Submitter-set TTL deadline; runtime-read-only. `none` means no TTL set. -/
  validUntil           : Option Time := none
  deriving Repr

namespace RequestContext

/-- Coherence relation between lifecycle state and admission state. -/
def coherentStateAdmission : RequestState → AdmissionState → Prop
  | .pending, a => a = .released
  | .claimed, a => a = .waiting ∨ a = .acquired
  | .processing, a => a = .executing
  | .inputRequired, a => a = .executing
  | .completed, a => a = .released
  | .failed, a => a = .released
  | .superseded, a => a = .released
  | .dead, a => a = .released
  | .interrupted, a => a = .released

instance (s : RequestState) (a : AdmissionState) : Decidable (coherentStateAdmission s a) := by
  cases s <;> unfold coherentStateAdmission <;> infer_instance

/-- State/admission coherence for a concrete request context. -/
def coherent (r : RequestContext) : Prop :=
  coherentStateAdmission r.state r.admission

instance (r : RequestContext) : Decidable r.coherent := by
  unfold coherent
  infer_instance

/-- Whether the deadline has been exceeded. -/
def deadlineExceeded (r : RequestContext) : Prop :=
  r.currentTime > r.deadline

instance (r : RequestContext) : Decidable r.deadlineExceeded :=
  Nat.decLt r.deadline r.currentTime

/-- Whether retries are exhausted. -/
def retriesExhausted (r : RequestContext) : Prop :=
  r.retryCount ≥ r.maxRetries

instance (r : RequestContext) : Decidable r.retriesExhausted :=
  Nat.decLe r.maxRetries r.retryCount

/-- Project any terminal transition into a released admission state. -/
def releaseToTerminal (r : RequestContext) (terminal : RequestState) : RequestContext :=
  match terminal with
  | .completed => { r with state := .completed, admission := .released, persistence := .committed }
  | .failed => { r with state := .failed, admission := .released }
  | .superseded => { r with state := .superseded, admission := .released }
  | .dead => { r with state := .dead, admission := .released }
  | .interrupted => { r with state := .interrupted, admission := .released }
  | .pending => { r with admission := .released }
  | .claimed => { r with admission := .released }
  | .processing => { r with admission := .released }
  | .inputRequired => { r with admission := .released }

theorem releaseToTerminal_state
    {r : RequestContext} {terminal : RequestState}
    (h_terminal : isTerminal terminal) :
    (releaseToTerminal r terminal).state = terminal := by
  cases h_terminal with
  | inl h => simp [releaseToTerminal, h]
  | inr h =>
    cases h with
    | inl h => simp [releaseToTerminal, h]
    | inr h =>
      cases h with
      | inl h => simp [releaseToTerminal, h]
      | inr h =>
        cases h with
        | inl h => simp [releaseToTerminal, h]
        | inr h => simp [releaseToTerminal, h]

theorem releaseToTerminal_released
    (r : RequestContext) (terminal : RequestState) :
    (releaseToTerminal r terminal).admission = .released := by
  cases terminal <;> simp [releaseToTerminal]

theorem releaseToTerminal_backend
    (r : RequestContext) (terminal : RequestState) :
    (releaseToTerminal r terminal).backend = r.backend := by
  cases terminal <;> simp [releaseToTerminal]

/-- Request lifecycle transitions. Each constructor encodes one valid
    state transition together with the resulting record update. -/
inductive Transition : RequestContext → RequestContext → Prop where
  | claim {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
      post = { pre with state := .claimed, admission := .waiting, claimTime := pre.currentTime, deadline := pre.currentTime + 1 } →
      Transition pre post
  | dedup_lose {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
      post = { pre with state := .superseded } →
      Transition pre post
  | begin_inference {pre post : RequestContext} :
      pre.state = .claimed →
      pre.admission = .acquired →
      post = { pre with state := .processing, admission := .executing } →
      Transition pre post
  | advance {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      post = { pre with progressSeq := pre.progressSeq + 1 } →
      Transition pre post
  | need_input {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      post = { pre with state := .inputRequired } →
      Transition pre post
  | input_received {pre post : RequestContext} :
      pre.state = .inputRequired →
      pre.admission = .executing →
      post = { pre with state := .processing, progressSeq := pre.progressSeq + 1 } →
      Transition pre post
  | finish {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      post = { pre with state := .completed, admission := .released, persistence := .committed } →
      Transition pre post
  | fail {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      post = { pre with state := .failed, admission := .released } →
      Transition pre post
  | fail_before_stream {pre post : RequestContext} :
      pre.state = .claimed →
      (pre.admission = .waiting ∨ pre.admission = .acquired) →
      post = { pre with state := .failed, admission := .released } →
      Transition pre post
  | input_timeout {pre post : RequestContext} :
      pre.state = .inputRequired →
      pre.admission = .executing →
      pre.deadlineExceeded →
      post = { pre with state := .failed, admission := .released } →
      Transition pre post
  | exhaust {pre post : RequestContext} :
      pre.state = .failed →
      (pre.retriesExhausted ∨ pre.deadlineExceeded) →
      post = { pre with state := .dead, admission := .released } →
      Transition pre post
  | deadline_expire {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      pre.deadlineExceeded →
      post = { pre with state := .dead, admission := .released } →
      Transition pre post
  | expire {pre post : RequestContext} {t : Time} :
      pre.state = .pending →
      pre.admission = .released →
      pre.validUntil = some t →
      pre.currentTime > t →
      post = { pre with state := .dead, admission := .released } →
      Transition pre post
  | interrupt_before_claim {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      Transition pre post
  | interrupt_claimed {pre post : RequestContext} :
      pre.state = .claimed →
      (pre.admission = .waiting ∨ pre.admission = .acquired) →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      Transition pre post
  | interrupt_processing {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      Transition pre post
  | interrupt_input_required {pre post : RequestContext} :
      pre.state = .inputRequired →
      pre.admission = .executing →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      Transition pre post

/-- Executable request actions mirroring `Transition`. -/
inductive Action where
  | claim
  | dedupLose
  | beginInference
  | advance
  | needInput
  | inputReceived
  | finish
  | fail
  | failBeforeStream
  | inputTimeout
  | exhaust
  | deadlineExpire
  | expire
  | interruptBeforeClaim
  | interruptClaimed
  | interruptProcessing
  | interruptInputRequired
  deriving DecidableEq, Repr

/-- Executable transition function for the request layer. -/
def step? (pre : RequestContext) : Action → Option RequestContext
  | .claim =>
      if pre.state = .pending ∧ pre.admission = .released then
        some { pre with state := .claimed, admission := .waiting, claimTime := pre.currentTime, deadline := pre.currentTime + 1 }
      else
        none
  | .dedupLose =>
      if pre.state = .pending ∧ pre.admission = .released then
        some { pre with state := .superseded }
      else
        none
  | .beginInference =>
      if pre.state = .claimed ∧ pre.admission = .acquired then
        some { pre with state := .processing, admission := .executing }
      else
        none
  | .advance =>
      if pre.state = .processing ∧ pre.admission = .executing then
        some { pre with progressSeq := pre.progressSeq + 1 }
      else
        none
  | .needInput =>
      if pre.state = .processing ∧ pre.admission = .executing then
        some { pre with state := .inputRequired }
      else
        none
  | .inputReceived =>
      if pre.state = .inputRequired ∧ pre.admission = .executing then
        some { pre with state := .processing, progressSeq := pre.progressSeq + 1 }
      else
        none
  | .finish =>
      if pre.state = .processing ∧ pre.admission = .executing then
        some { pre with state := .completed, admission := .released, persistence := .committed }
      else
        none
  | .fail =>
      if pre.state = .processing ∧ pre.admission = .executing then
        some { pre with state := .failed, admission := .released }
      else
        none
  | .failBeforeStream =>
      if pre.state = .claimed ∧ (pre.admission = .waiting ∨ pre.admission = .acquired) then
        some { pre with state := .failed, admission := .released }
      else
        none
  | .inputTimeout =>
      if pre.state = .inputRequired ∧ pre.admission = .executing ∧ pre.deadlineExceeded then
        some { pre with state := .failed, admission := .released }
      else
        none
  | .exhaust =>
      if pre.state = .failed ∧ (pre.retriesExhausted ∨ pre.deadlineExceeded) then
        some { pre with state := .dead, admission := .released }
      else
        none
  | .deadlineExpire =>
      if pre.state = .processing ∧ pre.admission = .executing ∧ pre.deadlineExceeded then
        some { pre with state := .dead, admission := .released }
      else
        none
  | .expire =>
      match pre.validUntil with
      | some t =>
          if pre.state = .pending ∧ pre.admission = .released ∧ pre.currentTime > t then
            some { pre with state := .dead, admission := .released }
          else
            none
      | none => none
  | .interruptBeforeClaim =>
      if pre.state = .pending ∧ pre.admission = .released ∧ pre.interruptRequestedAt.isSome then
        some { pre with state := .interrupted, admission := .released }
      else
        none
  | .interruptClaimed =>
      if pre.state = .claimed ∧ (pre.admission = .waiting ∨ pre.admission = .acquired)
         ∧ pre.interruptRequestedAt.isSome then
        some { pre with state := .interrupted, admission := .released }
      else
        none
  | .interruptProcessing =>
      if pre.state = .processing ∧ pre.admission = .executing
         ∧ pre.interruptRequestedAt.isSome then
        some { pre with state := .interrupted, admission := .released }
      else
        none
  | .interruptInputRequired =>
      if pre.state = .inputRequired ∧ pre.admission = .executing
         ∧ pre.interruptRequestedAt.isSome then
        some { pre with state := .interrupted, admission := .released }
      else
        none

/-- A trace is a sequence of valid request transitions. -/
inductive Trace : RequestContext → RequestContext → Prop where
  | refl {s : RequestContext} : Trace s s
  | step {s₁ s₂ s₃ : RequestContext} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

/-- Replay a finite action list through the executable request semantics. -/
def replay? : RequestContext → List Action → Option RequestContext
  | s, [] => some s
  | s, action :: rest =>
      match step? s action with
      | some s' => replay? s' rest
      | none => none

theorem step_sound
    {pre post : RequestContext}
    {action : Action}
    (h_step : step? pre action = some post) :
    Transition pre post := by
  cases action with
  | claim =>
      simp [step?] at h_step
      rcases h_step with ⟨h_claim, h_post⟩
      rcases h_claim with ⟨h_state, h_admission⟩
      exact Transition.claim h_state h_admission h_post.symm
  | dedupLose =>
      simp [step?] at h_step
      rcases h_step with ⟨h_claim, h_post⟩
      rcases h_claim with ⟨h_state, h_admission⟩
      exact Transition.dedup_lose h_state h_admission h_post.symm
  | beginInference =>
      simp [step?] at h_step
      rcases h_step with ⟨h_begin, h_post⟩
      rcases h_begin with ⟨h_state, h_admission⟩
      exact Transition.begin_inference h_state h_admission h_post.symm
  | advance =>
      simp [step?] at h_step
      rcases h_step with ⟨h_advance, h_post⟩
      rcases h_advance with ⟨h_state, h_admission⟩
      exact Transition.advance h_state h_admission h_post.symm
  | needInput =>
      simp [step?] at h_step
      rcases h_step with ⟨h_need, h_post⟩
      rcases h_need with ⟨h_state, h_admission⟩
      exact Transition.need_input h_state h_admission h_post.symm
  | inputReceived =>
      simp [step?] at h_step
      rcases h_step with ⟨h_input, h_post⟩
      rcases h_input with ⟨h_state, h_admission⟩
      exact Transition.input_received h_state h_admission h_post.symm
  | finish =>
      simp [step?] at h_step
      rcases h_step with ⟨h_finish, h_post⟩
      rcases h_finish with ⟨h_state, h_admission⟩
      exact Transition.finish h_state h_admission h_post.symm
  | fail =>
      simp [step?] at h_step
      rcases h_step with ⟨h_fail, h_post⟩
      rcases h_fail with ⟨h_state, h_admission⟩
      exact Transition.fail h_state h_admission h_post.symm
  | failBeforeStream =>
      simp [step?] at h_step
      rcases h_step with ⟨h_fail, h_post⟩
      rcases h_fail with ⟨h_state, h_admission⟩
      exact Transition.fail_before_stream h_state h_admission h_post.symm
  | inputTimeout =>
      simp [step?] at h_step
      rcases h_step with ⟨h_timeout, h_post⟩
      rcases h_timeout with ⟨h_state, h_admission, h_deadline⟩
      exact Transition.input_timeout h_state h_admission h_deadline h_post.symm
  | exhaust =>
      simp [step?] at h_step
      rcases h_step with ⟨h_exhaust, h_post⟩
      rcases h_exhaust with ⟨h_state, h_reason⟩
      exact Transition.exhaust h_state h_reason h_post.symm
  | deadlineExpire =>
      simp [step?] at h_step
      rcases h_step with ⟨h_dead, h_post⟩
      rcases h_dead with ⟨h_state, h_admission, h_deadline⟩
      exact Transition.deadline_expire h_state h_admission h_deadline h_post.symm
  | expire =>
      simp only [step?] at h_step
      match h_valid : pre.validUntil with
      | none =>
          rw [h_valid] at h_step
          simp at h_step
      | some t =>
          rw [h_valid] at h_step
          simp at h_step
          rcases h_step with ⟨⟨h_state, h_admission, h_time⟩, h_post⟩
          -- Rewrite `some t` back to `pre.validUntil` so the struct literal matches.
          rw [← h_valid] at h_post
          exact Transition.expire h_state h_admission h_valid h_time h_post.symm
  | interruptBeforeClaim =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_admission, h_int⟩, h_post⟩
      exact Transition.interrupt_before_claim h_state h_admission h_int h_post.symm
  | interruptClaimed =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_admission, h_int⟩, h_post⟩
      exact Transition.interrupt_claimed h_state h_admission h_int h_post.symm
  | interruptProcessing =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_admission, h_int⟩, h_post⟩
      exact Transition.interrupt_processing h_state h_admission h_int h_post.symm
  | interruptInputRequired =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_admission, h_int⟩, h_post⟩
      exact Transition.interrupt_input_required h_state h_admission h_int h_post.symm

theorem transition_complete
    {pre post : RequestContext}
    (h_trans : Transition pre post) :
    ∃ action : Action, step? pre action = some post := by
  cases h_trans with
  | claim h_state h_admission h_post =>
      exact ⟨.claim, by simp [step?, h_state, h_admission, h_post]⟩
  | dedup_lose h_state h_admission h_post =>
      exact ⟨.dedupLose, by simp [step?, h_state, h_admission, h_post]⟩
  | begin_inference h_state h_admission h_post =>
      exact ⟨.beginInference, by simp [step?, h_state, h_admission, h_post]⟩
  | advance h_state h_admission h_post =>
      exact ⟨.advance, by simp [step?, h_state, h_admission, h_post]⟩
  | need_input h_state h_admission h_post =>
      exact ⟨.needInput, by simp [step?, h_state, h_admission, h_post]⟩
  | input_received h_state h_admission h_post =>
      exact ⟨.inputReceived, by simp [step?, h_state, h_admission, h_post]⟩
  | finish h_state h_admission h_post =>
      exact ⟨.finish, by simp [step?, h_state, h_admission, h_post]⟩
  | fail h_state h_admission h_post =>
      exact ⟨.fail, by simp [step?, h_state, h_admission, h_post]⟩
  | fail_before_stream h_state h_admission h_post =>
      exact ⟨.failBeforeStream, by simp [step?, h_state, h_admission, h_post]⟩
  | input_timeout h_state h_admission h_deadline h_post =>
      exact ⟨.inputTimeout, by simp [step?, h_state, h_admission, h_deadline, h_post]⟩
  | exhaust h_state h_reason h_post =>
      exact ⟨.exhaust, by simp [step?, h_state, h_reason, h_post]⟩
  | deadline_expire h_state h_admission h_deadline h_post =>
      exact ⟨.deadlineExpire, by simp [step?, h_state, h_admission, h_deadline, h_post]⟩
  | expire h_state h_admission h_valid h_time h_post =>
      refine ⟨.expire, ?_⟩
      simp only [step?]
      rw [h_valid]
      simp [h_state, h_admission, h_time, h_post, h_valid]
  | interrupt_before_claim h_state h_admission h_int h_post =>
      exact ⟨.interruptBeforeClaim, by simp [step?, h_state, h_admission, h_int, h_post]⟩
  | interrupt_claimed h_state h_admission h_int h_post =>
      exact ⟨.interruptClaimed, by simp [step?, h_state, h_admission, h_int, h_post]⟩
  | interrupt_processing h_state h_admission h_int h_post =>
      exact ⟨.interruptProcessing, by simp [step?, h_state, h_admission, h_int, h_post]⟩
  | interrupt_input_required h_state h_admission h_int h_post =>
      exact ⟨.interruptInputRequired, by simp [step?, h_state, h_admission, h_int, h_post]⟩

theorem replay_sound
    {pre post : RequestContext}
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
    {pre post : RequestContext}
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

theorem terminal_implies_released_local
    {r : RequestContext}
    (h_coherent : r.coherent)
    (h_term : isTerminal r.state) :
    r.admission = .released := by
  cases r with
  | mk state origin backend admission deadline claimTime currentTime retryCount maxRetries progressSeq messageSeq isLatest persistence interruptRequestedAt validUntil =>
    cases h_term with
    | inl h =>
      cases h
      cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
      rfl
    | inr h =>
      cases h with
      | inl h =>
        cases h
        cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
        rfl
      | inr h =>
        cases h with
        | inl h =>
          cases h
          cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
          rfl
        | inr h =>
          cases h with
          | inl h =>
            cases h
            cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
            rfl
          | inr h =>
            cases h
            cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
            rfl

theorem backend_binding_preserved
    {pre post : RequestContext}
    (h_trans : Transition pre post) :
    post.backend = pre.backend := by
  cases h_trans with
  | claim _ _ h_post => rw [h_post]
  | dedup_lose _ _ h_post => rw [h_post]
  | begin_inference _ _ h_post => rw [h_post]
  | advance _ _ h_post => rw [h_post]
  | need_input _ _ h_post => rw [h_post]
  | input_received _ _ h_post => rw [h_post]
  | finish _ _ h_post => rw [h_post]
  | fail _ _ h_post => rw [h_post]
  | fail_before_stream _ _ h_post => rw [h_post]
  | input_timeout _ _ _ h_post => rw [h_post]
  | exhaust _ _ h_post => rw [h_post]
  | deadline_expire _ _ _ h_post => rw [h_post]
  | expire _ _ _ _ h_post => rw [h_post]
  | interrupt_before_claim _ _ _ h_post => rw [h_post]
  | interrupt_claimed _ _ _ h_post => rw [h_post]
  | interrupt_processing _ _ _ h_post => rw [h_post]
  | interrupt_input_required _ _ _ h_post => rw [h_post]

theorem origin_preserved
    {pre post : RequestContext}
    (h_trans : Transition pre post) :
    post.origin = pre.origin := by
  cases h_trans with
  | claim _ _ h_post => rw [h_post]
  | dedup_lose _ _ h_post => rw [h_post]
  | begin_inference _ _ h_post => rw [h_post]
  | advance _ _ h_post => rw [h_post]
  | need_input _ _ h_post => rw [h_post]
  | input_received _ _ h_post => rw [h_post]
  | finish _ _ h_post => rw [h_post]
  | fail _ _ h_post => rw [h_post]
  | fail_before_stream _ _ h_post => rw [h_post]
  | input_timeout _ _ _ h_post => rw [h_post]
  | exhaust _ _ h_post => rw [h_post]
  | deadline_expire _ _ _ h_post => rw [h_post]
  | expire _ _ _ _ h_post => rw [h_post]
  | interrupt_before_claim _ _ _ h_post => rw [h_post]
  | interrupt_claimed _ _ _ h_post => rw [h_post]
  | interrupt_processing _ _ _ h_post => rw [h_post]
  | interrupt_input_required _ _ _ h_post => rw [h_post]

theorem transition_produces_coherent
    {pre post : RequestContext}
    (h_trans : Transition pre post) :
    post.coherent := by
  cases h_trans with
  | claim _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | dedup_lose _ h_release h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission, h_release]
  | begin_inference _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | advance h_state h_admission h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission, h_state, h_admission]
  | need_input _ h_admission h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission, h_admission]
  | input_received _ h_admission h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission, h_admission]
  | finish _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | fail _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | fail_before_stream _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | input_timeout _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | exhaust _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | deadline_expire _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | expire _ _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | interrupt_before_claim _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | interrupt_claimed _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | interrupt_processing _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | interrupt_input_required _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]

theorem claimed_coherent_cases
    {r : RequestContext}
    (h_state : r.state = .claimed)
    (h_coherent : r.coherent) :
    r.admission = .waiting ∨ r.admission = .acquired := by
  cases r with
  | mk state origin backend admission deadline claimTime currentTime retryCount maxRetries progressSeq messageSeq isLatest persistence interruptRequestedAt validUntil =>
    cases h_state
    cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent ⊢

end RequestContext
