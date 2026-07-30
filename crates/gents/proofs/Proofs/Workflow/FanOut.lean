import Proofs.Background

namespace Workflow

open Subagent
open ToolExecution

def bridgeToolState (b : BridgedState) : ToolCallState :=
  match b.terminalOf with
  | .running => .running
  | .completed => .completed
  | t => t.projectedToolState

structure WorkflowGroup where
  groupId : ToolCallId
  groupTerminalStates : List ToolCallState
  synthesisPresent : Bool
  hne : groupTerminalStates ≠ []
  hwidth : groupTerminalStates.length ≤ Subagent.maxBackgroundedPerParent

namespace WorkflowGroup

def allTerminal (g : WorkflowGroup) : Prop :=
  ∀ s ∈ g.groupTerminalStates, isTerminal s

instance (g : WorkflowGroup) : Decidable g.allTerminal := by
  unfold allTerminal
  exact List.decidableBAll _ _

def allTerminalB (g : WorkflowGroup) : Bool :=
  g.groupTerminalStates.all (fun s => decide (isTerminal s))

theorem allTerminalB_iff (g : WorkflowGroup) :
    g.allTerminalB = true ↔ g.allTerminal := by
  unfold allTerminalB allTerminal
  simp [List.all_eq_true]

def barrierInvariant (g : WorkflowGroup) : Prop :=
  g.synthesisPresent = true → g.allTerminal

end WorkflowGroup

open WorkflowGroup

inductive Step : WorkflowGroup → WorkflowGroup → Prop where
  | terminalize
      (pre : WorkflowGroup)
      (idx : Nat)
      (t : ToolCallState)
      (h_running : pre.groupTerminalStates[idx]? = some .running)
      (h_terminal_target : isTerminal t)
      {post : WorkflowGroup}
      (h_states : post.groupTerminalStates = pre.groupTerminalStates.set idx t)
      (h_synth_eq : post.synthesisPresent = pre.synthesisPresent) :
      Step pre post
  | spawn_synthesis
      (pre : WorkflowGroup)
      (h_not_yet : pre.synthesisPresent = false)
      (h_guard : pre.allTerminal)
      {post : WorkflowGroup}
      (h_states : post.groupTerminalStates = pre.groupTerminalStates)
      (h_synth_set : post.synthesisPresent = true) :
      Step pre post

inductive Trace : WorkflowGroup → WorkflowGroup → Prop where
  | refl {g : WorkflowGroup} : Trace g g
  | step {g₁ g₂ g₃ : WorkflowGroup} :
      Step g₁ g₂ → Trace g₂ g₃ → Trace g₁ g₃

def Initial (g : WorkflowGroup) : Prop :=
  g.synthesisPresent = false ∧
  (∀ s ∈ g.groupTerminalStates, s = ToolCallState.running)

def Reachable (g : WorkflowGroup) : Prop :=
  ∃ init : WorkflowGroup, Initial init ∧ Trace init g

theorem Initial.not_allTerminal {g : WorkflowGroup}
    (h : Initial g) : ¬ g.allTerminal := by
  obtain ⟨_, h_running⟩ := h
  intro h_all
  cases h_head : g.groupTerminalStates with
  | nil => exact g.hne h_head
  | cons x xs =>
      have hx_mem : x ∈ g.groupTerminalStates := by simp [h_head]
      have hx_run : x = ToolCallState.running := h_running x hx_mem
      have hx_term : isTerminal x := h_all x hx_mem
      rw [hx_run] at hx_term
      simp [HasTerminal.isTerminal] at hx_term

theorem set_preserves_allTerminal
    {l : List ToolCallState} {idx : Nat} {t : ToolCallState}
    (h_t : isTerminal t)
    (h_all : ∀ s ∈ l, isTerminal s) :
    ∀ s ∈ l.set idx t, isTerminal s := by
  intro s hs
  rcases List.mem_or_eq_of_mem_set hs with hmem | rfl
  · exact h_all s hmem
  · exact h_t

theorem Step.preserves_barrier {pre post : WorkflowGroup}
    (h_inv : pre.barrierInvariant) (h_step : Step pre post) :
    post.barrierInvariant := by
  intro h_post_synth
  cases h_step with
  | terminalize idx t h_running h_t h_states h_synth_eq =>
      have h_pre_synth : pre.synthesisPresent = true := by
        rw [h_synth_eq] at h_post_synth; exact h_post_synth
      have h_pre_all : pre.allTerminal := h_inv h_pre_synth
      intro s hs
      rw [h_states] at hs
      exact set_preserves_allTerminal h_t h_pre_all s hs
  | spawn_synthesis h_not_yet h_guard h_states h_synth_set =>
      intro s hs
      rw [h_states] at hs
      exact h_guard s hs

theorem Trace.preserves_barrier {g g' : WorkflowGroup}
    (h_inv : g.barrierInvariant) (h_trace : Trace g g') :
    g'.barrierInvariant := by
  induction h_trace with
  | refl => exact h_inv
  | step h_step _ ih =>
      exact ih (h_step.preserves_barrier h_inv)

theorem Initial.barrierInvariant {g : WorkflowGroup}
    (h : Initial g) : g.barrierInvariant := by
  intro h_synth
  rw [h.1] at h_synth
  exact absurd h_synth (by decide)

theorem barrier_completeness {g : WorkflowGroup}
    (r : Reachable g) (h_synth : g.synthesisPresent = true) :
    g.allTerminal := by
  obtain ⟨init, h_init, h_trace⟩ := r
  exact (h_trace.preserves_barrier h_init.barrierInvariant) h_synth

theorem initial_has_running_witness {g : WorkflowGroup}
    (h : Initial g) : ¬ g.allTerminal :=
  h.not_allTerminal

end Workflow
