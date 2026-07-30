import Proofs.Request.State
import Proofs.ToolExecution.CancelCause
import Proofs.ToolExecution.State
import Proofs.Workflow.FanOut

namespace Workflow
namespace CompositeInterrupt

open ToolExecution
open RequestState

inductive Phase where
  | fanOutSpawn
  | fanOutBarrier
  | synthesisSpawn
  | synthesisRun
  | resultPersist
  deriving DecidableEq, Repr

namespace Phase

def toContract : Phase → String
  | .fanOutSpawn => "fanOutSpawn"
  | .fanOutBarrier => "fanOutBarrier"
  | .synthesisSpawn => "synthesisSpawn"
  | .synthesisRun => "synthesisRun"
  | .resultPersist => "resultPersist"

def all : List Phase :=
  [ .fanOutSpawn, .fanOutBarrier, .synthesisSpawn, .synthesisRun, .resultPersist ]

theorem all_complete (p : Phase) : p ∈ all := by
  cases p <;> simp [all]

end Phase

structure State where
  parentState : RequestState
  outerState : ToolCallState
  outerCancelCause : Option CancelCause
  phase : Phase
  fanOutBridges : List ToolCallState
  synthesisBridge : Option ToolCallState
  continuationOwned : Bool
  pendingChildCleanup : Bool
  deriving Repr

namespace State

def outerEligibleActive (s : State) : Prop :=
  s.outerState = .running ∨ s.outerState = .pending ∨ s.outerState = .awaitingApproval

instance (s : State) : Decidable s.outerEligibleActive := by
  unfold outerEligibleActive; infer_instance

def outerEligibleActiveB (s : State) : Bool :=
  decide (s.outerEligibleActive)

def parentTerminal (s : State) : Prop :=
  isTerminal s.parentState

instance (s : State) : Decidable s.parentTerminal := by
  unfold parentTerminal; infer_instance

def cleanupInvariant (s : State) : Prop :=
  s.parentTerminal → ¬ s.outerEligibleActive

instance (s : State) : Decidable s.cleanupInvariant := by
  unfold cleanupInvariant; infer_instance

def cleanupInvariantB (s : State) : Bool :=
  decide (s.cleanupInvariant)

def cancelCauseConsistent (s : State) : Prop :=
  s.outerState = .cancelled → s.outerCancelCause = some .interrupted

instance (s : State) : Decidable s.cancelCauseConsistent := by
  unfold cancelCauseConsistent; infer_instance

def interruptTerminal (s : State) : Prop :=
  s.outerState = .cancelled ∧
  s.outerCancelCause = some .interrupted ∧
  s.continuationOwned = false

instance (s : State) : Decidable s.interruptTerminal := by
  unfold interruptTerminal; infer_instance

def interruptCleanupPost (pre : State) : State :=
  { pre with
    parentState := .interrupted
  , outerState :=
      if pre.outerEligibleActive then .cancelled else pre.outerState
  , outerCancelCause :=
      if pre.outerEligibleActive then some .interrupted
      else pre.outerCancelCause
  , continuationOwned := false
  , pendingChildCleanup :=
      pre.pendingChildCleanup ||
        (pre.fanOutBridges.any (fun b => !decide (isTerminal b)) ||
          match pre.synthesisBridge with
          | some b => !decide (isTerminal b)
          | none => false) }

def recoverTerminalParentPost (pre : State) : State :=
  let interrupted := pre.parentState = .interrupted
  { pre with
    outerState := if interrupted then .cancelled else .failed
  , outerCancelCause := if interrupted then some .interrupted else none
  , continuationOwned := false
  , pendingChildCleanup :=
      pre.pendingChildCleanup ||
        (pre.fanOutBridges.any (fun b => !decide (isTerminal b)) ||
          match pre.synthesisBridge with
          | some b => !decide (isTerminal b)
          | none => false) }

def finishChildCleanupPost (pre : State) : State :=
  { pre with
    pendingChildCleanup := false
  , fanOutBridges := pre.fanOutBridges.map (fun b =>
      if isTerminal b then b else .cancelled)
  , synthesisBridge :=
      match pre.synthesisBridge with
      | some b => some (if isTerminal b then b else .cancelled)
      | none => none }

end State

open State

def Initial (s : State) : Prop :=
  s.parentState = .processing ∧
  s.outerState = .running ∧
  s.outerCancelCause = none ∧
  s.phase = .fanOutSpawn ∧
  (∀ b ∈ s.fanOutBridges, b = .running) ∧
  s.synthesisBridge = none ∧
  s.continuationOwned = true ∧
  s.pendingChildCleanup = false

inductive Step : State → State → Prop where
  | advancePhase
      (pre : State)
      (next : Phase)
      (h_parent_live : ¬ isTerminal pre.parentState)
      (h_outer_running : pre.outerState = .running)
      (h_owned : pre.continuationOwned = true)
      {post : State}
      (h_post : post = { pre with phase := next }) :
      Step pre post

  | terminalizeFanOut
      (pre : State)
      (idx : Nat)
      (t : ToolCallState)
      (h_parent_live : ¬ isTerminal pre.parentState)
      (h_running : pre.fanOutBridges[idx]? = some .running)
      (h_terminal : isTerminal t)
      {post : State}
      (h_post : post =
        { pre with fanOutBridges := pre.fanOutBridges.set idx t }) :
      Step pre post

  | spawnSynthesis
      (pre : State)
      (h_parent_live : ¬ isTerminal pre.parentState)
      (h_outer_running : pre.outerState = .running)
      (h_phase : pre.phase = .synthesisSpawn)
      (h_absent : pre.synthesisBridge = none)
      (h_all_terminal : ∀ b ∈ pre.fanOutBridges, isTerminal b)
      {post : State}
      (h_post : post =
        { pre with
          synthesisBridge := some .running
        , phase := .synthesisRun }) :
      Step pre post

  | terminalizeSynthesis
      (pre : State)
      (t : ToolCallState)
      (h_parent_live : ¬ isTerminal pre.parentState)
      (h_running : pre.synthesisBridge = some .running)
      (h_terminal : isTerminal t)
      {post : State}
      (h_post : post = { pre with synthesisBridge := some t }) :
      Step pre post

  | completeOuter
      (pre : State)
      (h_parent_live : ¬ isTerminal pre.parentState)
      (h_outer_running : pre.outerState = .running)
      (h_owned : pre.continuationOwned = true)
      {post : State}
      (h_post : post =
        { pre with
          outerState := .completed
        , continuationOwned := false
        , phase := .resultPersist }) :
      Step pre post

  | interruptCleanup
      (pre : State)
      (h_parent_live_or_interrupted :
        pre.parentState = .processing ∨ pre.parentState = .interrupted)
      {post : State}
      (h_post : post = interruptCleanupPost pre) :
      Step pre post

  | finishChildCleanup
      (pre : State)
      (h_interrupt : interruptTerminal pre)
      (h_pending : pre.pendingChildCleanup = true)
      {post : State}
      (h_post : post = finishChildCleanupPost pre) :
      Step pre post

  | lateCompleteRefused
      (pre : State)
      (h_cancelled : pre.outerState = .cancelled)
      {post : State}
      (h_post : post = pre) :
      Step pre post

  | recoverTerminalParent
      (pre : State)
      (h_parent_terminal : isTerminal pre.parentState)
      (h_outer_running : pre.outerState = .running)
      {post : State}
      (h_post : post = recoverTerminalParentPost pre) :
      Step pre post

inductive Trace : State → State → Prop where
  | refl {s : State} : Trace s s
  | step {s₁ s₂ s₃ : State} : Step s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

def Reachable (s : State) : Prop :=
  ∃ init : State, Initial init ∧ Trace init s

theorem interrupt_cleanup_terminalizes_outer (pre : State) :
    let post := interruptCleanupPost pre
    ¬ post.outerEligibleActive ∧
    post.parentState = .interrupted ∧
    post.continuationOwned = false ∧
    (pre.outerEligibleActive → interruptTerminal post) := by
  intro post
  refine ⟨?_, rfl, rfl, ?_⟩
  · intro h_elig
    unfold outerEligibleActive at h_elig
    by_cases h_pre : pre.outerEligibleActive
    ·
      have h_out : post.outerState = .cancelled := by
        simp only [post, interruptCleanupPost, h_pre, ite_true]
      rw [h_out] at h_elig
      rcases h_elig with h | h | h <;> exact absurd h (by decide)
    ·
      have h_out : post.outerState = pre.outerState := by
        simp only [post, interruptCleanupPost, h_pre, ite_false]
      rw [h_out] at h_elig
      exact h_pre (by simpa [outerEligibleActive] using h_elig)
  · intro h_pre
    simp [post, interruptCleanupPost, interruptTerminal, h_pre]

theorem interrupt_cleanup_idempotent
    (pre : State)
    (h_term : interruptTerminal pre) :
    let post := interruptCleanupPost pre
    post.outerState = pre.outerState ∧
    post.outerCancelCause = pre.outerCancelCause ∧
    post.continuationOwned = false := by
  have h_not_elig : ¬ pre.outerEligibleActive := by
    intro h
    rcases h_term with ⟨h_canc, _, _⟩
    simp [outerEligibleActive, h_canc] at h
  intro post
  simp [post, interruptCleanupPost, h_not_elig]

theorem finish_child_cleanup_preserves_outer_terminal
    (pre : State)
    (h_term : interruptTerminal pre) :
    let post := finishChildCleanupPost pre
    interruptTerminal post ∧ post.pendingChildCleanup = false := by
  intro post
  refine ⟨?_, rfl⟩
  rcases h_term with ⟨h1, h2, h3⟩
  simp [post, finishChildCleanupPost, interruptTerminal, h1, h2, h3]

theorem late_complete_refused_preserves
    (pre : State)
    (h_canc : pre.outerState = .cancelled)
    (post : State)
    (h_post : post = pre) :
    post = pre ∧ post.outerState = .cancelled := by
  subst h_post
  exact ⟨rfl, h_canc⟩

theorem recover_terminal_parent_cancels_outer
    (pre : State)
    (_h_parent : isTerminal pre.parentState)
    (_h_outer : pre.outerState = .running) :
    let post := recoverTerminalParentPost pre
    ¬ post.outerEligibleActive ∧
    isTerminal post.outerState ∧
    post.continuationOwned = false ∧
    (pre.parentState = .interrupted →
      post.outerState = .cancelled ∧ post.outerCancelCause = some .interrupted) := by
  intro post
  refine ⟨?_, ?_, rfl, ?_⟩
  · intro h
    by_cases h_int : pre.parentState = .interrupted
    · simp [post, recoverTerminalParentPost, h_int, outerEligibleActive] at h
    · simp [post, recoverTerminalParentPost, h_int, outerEligibleActive] at h
  · by_cases h_int : pre.parentState = .interrupted
    · simp [post, recoverTerminalParentPost, h_int, HasTerminal.isTerminal,
        ToolCallState.instHasTerminal]
    · simp [post, recoverTerminalParentPost, h_int, HasTerminal.isTerminal,
        ToolCallState.instHasTerminal]
  · intro h_int
    simp [post, recoverTerminalParentPost, h_int]

theorem Step.preserves_interrupt_terminal
    {pre post : State}
    (h_term : interruptTerminal pre)
    (h_step : Step pre post) :
    interruptTerminal post := by
  rcases h_term with ⟨h_canc, h_cause, h_owned⟩
  cases h_step with
  | advancePhase next h_parent h_outer h_owned' h_post =>
      rw [h_canc] at h_outer; cases h_outer
  | terminalizeFanOut idx t h_parent h_running h_t h_post =>
      subst h_post
      exact ⟨h_canc, h_cause, h_owned⟩
  | spawnSynthesis h_parent h_outer h_phase h_absent h_all h_post =>
      rw [h_canc] at h_outer; cases h_outer
  | terminalizeSynthesis t h_parent h_running h_t h_post =>
      subst h_post
      exact ⟨h_canc, h_cause, h_owned⟩
  | completeOuter h_parent h_outer h_owned' h_post =>
      rw [h_canc] at h_outer; cases h_outer
  | interruptCleanup h_parent h_post =>
      have h_not_elig : ¬ pre.outerEligibleActive := by
        intro h
        simp [outerEligibleActive, h_canc] at h
      subst h_post
      simp [interruptCleanupPost, interruptTerminal, h_not_elig, h_canc, h_cause]
  | finishChildCleanup h_term' h_pending h_post =>
      subst h_post
      exact ⟨h_canc, h_cause, h_owned⟩
  | lateCompleteRefused h_canc' h_post =>
      subst h_post
      exact ⟨h_canc, h_cause, h_owned⟩
  | recoverTerminalParent h_parent h_outer h_post =>
      rw [h_canc] at h_outer; cases h_outer

theorem Trace.preserves_interrupt_terminal
    {pre post : State}
    (h_term : interruptTerminal pre)
    (h_trace : Trace pre post) :
    interruptTerminal post := by
  induction h_trace with
  | refl => exact h_term
  | step h_step _ ih =>
      exact ih (h_step.preserves_interrupt_terminal h_term)

theorem interrupt_post_satisfies_cleanup_invariant (pre : State) :
    cleanupInvariant (interruptCleanupPost pre) := by
  intro _
  exact (interrupt_cleanup_terminalizes_outer pre).1

theorem Initial.outer_eligible {s : State} (h : Initial s) :
    s.outerEligibleActive ∧ ¬ s.parentTerminal := by
  rcases h with ⟨h_parent, h_outer, _, _, _, _, _, _⟩
  refine ⟨?_, ?_⟩
  · simp [outerEligibleActive, h_outer]
  · simp [parentTerminal, h_parent, HasTerminal.isTerminal]

theorem terminal_parent_no_active_outer_after_cleanup
    (pre : State)
    (h_parent : pre.parentState = .processing ∨ pre.parentState = .interrupted) :
    ∃ post : State,
      Step pre post ∧
      post.parentState = .interrupted ∧
      ¬ post.outerEligibleActive ∧
      post.continuationOwned = false ∧
      (pre.outerEligibleActive →
        post.outerState = .cancelled ∧
        post.outerCancelCause = some .interrupted) := by
  let post := interruptCleanupPost pre
  have h_step : Step pre post :=
    Step.interruptCleanup pre h_parent (post := post) rfl
  have h_facts := interrupt_cleanup_terminalizes_outer pre
  refine ⟨post, h_step, rfl, h_facts.1, rfl, ?_⟩
  intro h_elig
  have h_term := h_facts.2.2.2 h_elig
  exact ⟨h_term.1, h_term.2.1⟩

theorem recover_establishes_cleanup_invariant
    (pre : State)
    (h_parent : isTerminal pre.parentState)
    (h_outer : pre.outerState = .running) :
    ∃ post : State,
      Step pre post ∧
      cleanupInvariant post ∧
      isTerminal post.outerState := by
  let post := recoverTerminalParentPost pre
  have h_step : Step pre post :=
    Step.recoverTerminalParent pre h_parent h_outer (post := post) rfl
  have h_facts := recover_terminal_parent_cancels_outer pre h_parent h_outer
  refine ⟨post, h_step, ?_, h_facts.2.1⟩
  intro _
  exact h_facts.1

end CompositeInterrupt
end Workflow
