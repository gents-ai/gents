import Proofs.Request.State
import Proofs.ToolExecution.CancelCause
import Proofs.ToolExecution.State
import Proofs.Workflow.FanOut

/-!
# Workflow.CompositeInterrupt

Model for the outer `fan_out_and_synthesize` composite tool call under parent
interrupt (#837).

## Broken invariant (pre-fix)

The outer composite `AgentToolCall` is owned as a *local* lifecycle while its
fan-out/synthesis workflow runs. Child bridges live in the cancel-visible map.
On parent interrupt the map drains and children terminalize, but the outer row
can remain `running` until deadline or daemon restart.

## What this model proves

A composite workflow is an evolving record: parent request state, outer tool
state, phase, child/synthesis projected states, continuation ownership, and
pending child cleanup. The bad state — parent terminal while outer is eligible
as active — is *representable*. Bounded interrupt cleanup excludes it, and
late normal terminalization cannot overwrite an interrupt terminal.

Meaningfulness comes from step preservation (and its trace lift), not from
baking "outer is cancelled" into a constructor premise.
-/

namespace Workflow
namespace CompositeInterrupt

open ToolExecution
open RequestState

/-- Phases of the composite workflow where interrupt may land. -/
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

/-- Evolving composite-orchestration state owned by one parent request. -/
structure State where
  parentState : RequestState
  outerState : ToolCallState
  outerCancelCause : Option CancelCause
  phase : Phase
  fanOutBridges : List ToolCallState
  synthesisBridge : Option ToolCallState
  /-- In-memory barrier/continuation ownership still held by the workflow. -/
  continuationOwned : Bool
  /-- Best-effort child/bridge cleanup still pending (retryable). -/
  pendingChildCleanup : Bool
  deriving Repr

namespace State

/-- Outer composite is eligible as active for liveness / status. -/
def outerEligibleActive (s : State) : Prop :=
  s.outerState = .running ∨ s.outerState = .pending ∨ s.outerState = .awaitingApproval

instance (s : State) : Decidable s.outerEligibleActive := by
  unfold outerEligibleActive; infer_instance

/-- Computable mirror of `outerEligibleActive`. -/
def outerEligibleActiveB (s : State) : Bool :=
  decide (s.outerEligibleActive)

/-- Parent has reached a terminal request state. -/
def parentTerminal (s : State) : Prop :=
  isTerminal s.parentState

instance (s : State) : Decidable s.parentTerminal := by
  unfold parentTerminal; infer_instance

/-- The post-interrupt cleanup invariant:
    if the parent is terminal, the outer composite is not eligible as active. -/
def cleanupInvariant (s : State) : Prop :=
  s.parentTerminal → ¬ s.outerEligibleActive

instance (s : State) : Decidable s.cleanupInvariant := by
  unfold cleanupInvariant; infer_instance

def cleanupInvariantB (s : State) : Bool :=
  decide (s.cleanupInvariant)

/-- Cancel-cause projection is consistent when the outer is cancelled. -/
def cancelCauseConsistent (s : State) : Prop :=
  s.outerState = .cancelled → s.outerCancelCause = some .interrupted

instance (s : State) : Decidable s.cancelCauseConsistent := by
  unfold cancelCauseConsistent; infer_instance

/-- Outer has a single interrupt terminal with released continuation. -/
def interruptTerminal (s : State) : Prop :=
  s.outerState = .cancelled ∧
  s.outerCancelCause = some .interrupted ∧
  s.continuationOwned = false

instance (s : State) : Decidable s.interruptTerminal := by
  unfold interruptTerminal; infer_instance

/-- Pure post-state constructor for bounded interrupt cleanup. -/
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

/-- Pure post-state constructor for terminal-parent recovery.

    Mirrors Rust `recover_stuck_running_tool_calls` / live reconcile:
    interrupted parent → cancelled+interrupted; other terminal parent → failed. -/
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

/-- Pure post-state for best-effort child cleanup after outer interrupt. -/
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

/-- Initial post-`start_running` composite: parent processing, outer running,
    ownership held, no synthesis yet, cleanup not pending. -/
def Initial (s : State) : Prop :=
  s.parentState = .processing ∧
  s.outerState = .running ∧
  s.outerCancelCause = none ∧
  s.phase = .fanOutSpawn ∧
  (∀ b ∈ s.fanOutBridges, b = .running) ∧
  s.synthesisBridge = none ∧
  s.continuationOwned = true ∧
  s.pendingChildCleanup = false

/-- Small-step relation. `interruptCleanup` is the bounded parent-interrupt
    transition: parent becomes interrupted, outer terminalizes exactly once if
    still eligible, continuation ownership is released, and child cleanup may
    remain pending without keeping the outer active. -/
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

  /-- Normal successful completion of the outer composite. Guarded on outer
      still running so an interrupt terminal cannot be overwritten. -/
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

  /-- Bounded interrupt cleanup. Representable from any phase while outer is
      still eligible; duplicate delivery is a no-op via already-terminal outer. -/
  | interruptCleanup
      (pre : State)
      (h_parent_live_or_interrupted :
        pre.parentState = .processing ∨ pre.parentState = .interrupted)
      {post : State}
      (h_post : post = interruptCleanupPost pre) :
      Step pre post

  /-- Best-effort child cleanup after outer is already interrupt-terminal.
      Must not re-activate the outer composite. -/
  | finishChildCleanup
      (pre : State)
      (h_interrupt : interruptTerminal pre)
      (h_pending : pre.pendingChildCleanup = true)
      {post : State}
      (h_post : post = finishChildCleanupPost pre) :
      Step pre post

  /-- Late normal completion after interrupt: refused. State unchanged.
      Models CAS-lost complete/fail against an already-cancelled outer. -/
  | lateCompleteRefused
      (pre : State)
      (h_cancelled : pre.outerState = .cancelled)
      {post : State}
      (h_post : post = pre) :
      Step pre post

  /-- Startup / live recovery from the representable bad durable state:
      parent already terminal, outer still running. -/
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

/-! ## Properties -/

/-- The post-state of `interruptCleanup` never leaves the outer eligible as
    active, always marks the parent interrupted, and releases ownership. -/
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
    · -- interrupt forces outer to cancelled, which is never eligible.
      have h_out : post.outerState = .cancelled := by
        simp only [post, interruptCleanupPost, h_pre, ite_true]
      rw [h_out] at h_elig
      rcases h_elig with h | h | h <;> exact absurd h (by decide)
    · -- outer state unchanged and was not eligible pre.
      have h_out : post.outerState = pre.outerState := by
        simp only [post, interruptCleanupPost, h_pre, ite_false]
      rw [h_out] at h_elig
      exact h_pre (by simpa [outerEligibleActive] using h_elig)
  · intro h_pre
    simp [post, interruptCleanupPost, interruptTerminal, h_pre]

/-- Duplicate interrupt is idempotent on an already interrupt-terminal state. -/
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

/-- Child cleanup after interrupt does not re-activate the outer. -/
theorem finish_child_cleanup_preserves_outer_terminal
    (pre : State)
    (h_term : interruptTerminal pre) :
    let post := finishChildCleanupPost pre
    interruptTerminal post ∧ post.pendingChildCleanup = false := by
  intro post
  refine ⟨?_, rfl⟩
  rcases h_term with ⟨h1, h2, h3⟩
  simp [post, finishChildCleanupPost, interruptTerminal, h1, h2, h3]

/-- Late complete against a cancelled outer is a no-op. -/
theorem late_complete_refused_preserves
    (pre : State)
    (h_canc : pre.outerState = .cancelled)
    (post : State)
    (h_post : post = pre) :
    post = pre ∧ post.outerState = .cancelled := by
  subst h_post
  exact ⟨rfl, h_canc⟩

/-- Recovery from terminal-parent / running-outer converges to a non-active
    outer terminal (cancelled if parent interrupted, else failed). -/
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

/-- `Step` preserves: once outer is cancelled with interrupted cause, it stays
    that way and never becomes eligible again. -/
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

/-- Trace lift of `Step.preserves_interrupt_terminal`. -/
theorem Trace.preserves_interrupt_terminal
    {pre post : State}
    (h_term : interruptTerminal pre)
    (h_trace : Trace pre post) :
    interruptTerminal post := by
  induction h_trace with
  | refl => exact h_term
  | step h_step _ ih =>
      exact ih (h_step.preserves_interrupt_terminal h_term)

/-- Explicit cleanup invariant after interrupt post-state construction. -/
theorem interrupt_post_satisfies_cleanup_invariant (pre : State) :
    cleanupInvariant (interruptCleanupPost pre) := by
  intro _
  exact (interrupt_cleanup_terminalizes_outer pre).1

/-- Initial state is well-formed for non-vacuity: outer is eligible while
    parent is live. -/
theorem Initial.outer_eligible {s : State} (h : Initial s) :
    s.outerEligibleActive ∧ ¬ s.parentTerminal := by
  rcases h with ⟨h_parent, h_outer, _, _, _, _, _, _⟩
  refine ⟨?_, ?_⟩
  · simp [outerEligibleActive, h_outer]
  · simp [parentTerminal, h_parent, HasTerminal.isTerminal]

/-- **Main safety:** a single interrupt cleanup from a live-or-already-
    interrupted parent yields a state where no owned outer remains eligible. -/
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

/-- Recovery establishes the cleanup invariant when the parent is already
    terminal and the outer is still running. -/
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
