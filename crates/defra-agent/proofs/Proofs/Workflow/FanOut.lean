import Proofs.Background

/-!
# Workflow.FanOut

Lean model for the `fan_out_and_synthesize` barrier obligation.

The runtime persists one `AgentToolCall` bridge per fan-out child. A workflow
group is the non-empty, bounded set of those bridges sharing one
`workflow_group_id`; the synthesis bridge may be spawned only after every
fan-out bridge is terminal in the parent-visible bridge vocabulary.

## Why this model has content (anti-T5)

A prior version made the barrier *assumed*: the only `Reachable` constructor
that introduced synthesis carried `allTerminal` as a premise, and the theorem
goal then reduced definitionally to that premise — it excluded no reachable
state, because "synthesis present with a non-terminal bridge" was not even
*representable*.

Here the group is an evolving record (`WorkflowGroup`): each fan-out bridge
carries a projected `ToolCallState` that *starts running* and *transitions to a
terminal state*, and `synthesisPresent` is a separate `Bool` flag. The bad
state — `synthesisPresent = true` together with some non-terminal bridge — is a
perfectly well-formed `WorkflowGroup` value. The barrier becomes a *derived*
invariant of the transition system: we must do induction over a `Trace` and
lean on monotonicity of terminalization to *exclude* that representable bad
state from the reachable set.

## Correspondence to `Subagent.Transition`

`groupTerminalStates[i]` is `Subagent.bridgeToolState` of the `i`-th fan-out
bridge (see `bridgeToolState` below). A `Step.terminalize` move corresponds to
one `Subagent.BridgedState.Transition.bridge_complete` (child `.completed`,
projected to `ToolCallState.completed`) or `.bridge_failure` (child non-completed
terminal, projected via `ChildTerminal.projectedToolState`:
`interrupted -> cancelled`, every other failure `-> failed`) firing on that
bridge: it advances exactly one bridge's parent-visible projected state from
`.running` to a terminal `ToolCallState`, leaving the rest of the group fixed.
`Step.spawn_synthesis` corresponds to the runtime's synthesis-bridge spawn,
guarded on every fan-out bridge already being terminal.
-/

namespace Workflow

open Subagent
open ToolExecution

/-- Parent-visible bridge state for a fan-out child.

The completed path is projected by the bridge-complete transition. Every
non-completed terminal goes through `ChildTerminal.projectedToolState`, matching
the runtime bridge-failure projection (`interrupted -> cancelled`, other
failures -> failed). This is the projection whose image populates a
`WorkflowGroup.groupTerminalStates` entry. -/
def bridgeToolState (b : BridgedState) : ToolCallState :=
  match b.terminalOf with
  | .running => .running
  | .completed => .completed
  | t => t.projectedToolState

/-- A fan-out group as an *evolving* state.

`groupId` is the durable `workflow_group_id`, equal to the
`fan_out_and_synthesize` orchestration tool call id in Rust.

`groupTerminalStates` is the per-bridge parent-visible projected
`ToolCallState` (one entry per fan-out child, in `bridgeToolState`'s image).
These *start running* (initial state) and individually *transition to terminal*
via `Step.terminalize`.

`synthesisPresent` is a separate flag: `true` once the synthesis bridge has been
spawned. The pair `(synthesisPresent := true, some non-terminal entry)` is
representable — that is exactly the bad state the barrier theorem must exclude
from the reachable set.

`hne` rules out the vacuous `N = 0` barrier; `hwidth` reuses the
backgrounded-per-parent cap. -/
structure WorkflowGroup where
  groupId : ToolCallId
  groupTerminalStates : List ToolCallState
  synthesisPresent : Bool
  hne : groupTerminalStates ≠ []
  hwidth : groupTerminalStates.length ≤ Subagent.maxBackgroundedPerParent

namespace WorkflowGroup

/-- Every fan-out bridge has reached a terminal projected state. -/
def allTerminal (g : WorkflowGroup) : Prop :=
  ∀ s ∈ g.groupTerminalStates, isTerminal s

instance (g : WorkflowGroup) : Decidable g.allTerminal := by
  unfold allTerminal
  exact List.decidableBAll _ _

/-- Computable mirror of `allTerminal`, used by the conformance decision
    procedure (`groupTerminalStates.all isTerminal`). -/
def allTerminalB (g : WorkflowGroup) : Bool :=
  g.groupTerminalStates.all (fun s => decide (isTerminal s))

theorem allTerminalB_iff (g : WorkflowGroup) :
    g.allTerminalB = true ↔ g.allTerminal := by
  unfold allTerminalB allTerminal
  simp [List.all_eq_true]

/-- The barrier invariant a reachable group must satisfy:
    *if synthesis is present, every fan-out bridge is terminal.* This is the
    representable property the transition system must preserve; it is `False`
    on the bad state (synthesis present ∧ some non-terminal bridge). -/
def barrierInvariant (g : WorkflowGroup) : Prop :=
  g.synthesisPresent = true → g.allTerminal

end WorkflowGroup

open WorkflowGroup

/-- Small-step relation on workflow groups.

`terminalize` advances exactly one fan-out bridge from `.running` to a terminal
projected `ToolCallState` (the `bridge_complete` / `bridge_failure`
correspondence documented in the module header). `spawn_synthesis` is *guarded*
by every fan-out bridge already being terminal, and sets `synthesisPresent`.

Crucially, neither constructor can move a terminal state back to non-terminal,
and the *only* way to set `synthesisPresent := true` is the guarded constructor.
Monotonicity of `terminalize` (terminal stays terminal; running may become
terminal) is what makes the barrier a derivable invariant rather than an
assumed one. -/
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

/-- Reflexive-transitive closure of `Step`. -/
inductive Trace : WorkflowGroup → WorkflowGroup → Prop where
  | refl {g : WorkflowGroup} : Trace g g
  | step {g₁ g₂ g₃ : WorkflowGroup} :
      Step g₁ g₂ → Trace g₂ g₃ → Trace g₁ g₃

/-- An *initial* fan-out group: every fan-out bridge running, no synthesis yet.
    This is the post-spawn state of `fan_out_and_synthesize` before any child
    has reported terminal. -/
def Initial (g : WorkflowGroup) : Prop :=
  g.synthesisPresent = false ∧
  (∀ s ∈ g.groupTerminalStates, s = ToolCallState.running)

/-- Reachable workflow groups: those reachable by a `Trace` from some `Initial`
    group. Note this is NOT a freestanding inductive whose synthesis constructor
    bakes in `allTerminal`; the barrier is recovered below by induction. -/
def Reachable (g : WorkflowGroup) : Prop :=
  ∃ init : WorkflowGroup, Initial init ∧ Trace init g

/-! ## Monotonicity: `Step` preserves the barrier invariant -/

/-- A non-empty all-running group is non-empty and not all-terminal-vacuous:
    its first entry exists and is `.running`, which is *not* terminal. This is
    where `hne` (1 ≤ N) earns its keep — an `N = 0` group could not be `Initial`
    and yet vacuously satisfy `allTerminal`, so the barrier would be content-free
    on it. Concretely: a non-empty `Initial` group is NOT `allTerminal`. -/
theorem Initial.not_allTerminal {g : WorkflowGroup}
    (h : Initial g) : ¬ g.allTerminal := by
  obtain ⟨_, h_running⟩ := h
  -- `hne` gives a head element; `Initial` forces it `.running`, which is not terminal.
  intro h_all
  cases h_head : g.groupTerminalStates with
  | nil => exact g.hne h_head
  | cons x xs =>
      have hx_mem : x ∈ g.groupTerminalStates := by simp [h_head]
      have hx_run : x = ToolCallState.running := h_running x hx_mem
      have hx_term : isTerminal x := h_all x hx_mem
      rw [hx_run] at hx_term
      -- `.running` is not terminal
      simp [HasTerminal.isTerminal] at hx_term

/-- The set/update used by `terminalize` is monotone in terminality: if every
    entry of the original list (other than the changed slot, which becomes the
    terminal target `t`) is mapped to terminal, then the updated list is
    all-terminal. Concretely: if `t` is terminal and we know `allTerminal` of
    the *pre* list, the *post* list (`pre.set idx t`) is all-terminal. -/
theorem set_preserves_allTerminal
    {l : List ToolCallState} {idx : Nat} {t : ToolCallState}
    (h_t : isTerminal t)
    (h_all : ∀ s ∈ l, isTerminal s) :
    ∀ s ∈ l.set idx t, isTerminal s := by
  intro s hs
  rcases List.mem_or_eq_of_mem_set hs with hmem | rfl
  · exact h_all s hmem
  · exact h_t

/-- **Monotonicity / invariant preservation.** A single `Step` preserves the
    barrier invariant. This is the load-bearing lemma:

    * `terminalize` can only turn a non-synthesis group into another, or turn a
      barrier-respecting group into one whose bridge set got *more* terminal —
      it never un-terminalizes, and it never sets synthesis. So if synthesis was
      present, it stayed present *and* the (already all-terminal, by the pre
      invariant) bridge set only gained terminality.
    * `spawn_synthesis` sets synthesis, but its guard `h_guard : allTerminal`
      establishes the conclusion directly, and `terminalize` cannot have
      occurred to break it afterward (that is the inductive step below). -/
theorem Step.preserves_barrier {pre post : WorkflowGroup}
    (h_inv : pre.barrierInvariant) (h_step : Step pre post) :
    post.barrierInvariant := by
  intro h_post_synth
  cases h_step with
  | terminalize idx t h_running h_t h_states h_synth_eq =>
      -- synthesis is unchanged; if it's present post, it was present pre,
      -- so the pre invariant gives `allTerminal pre`, and `set` monotonicity
      -- lifts it to post.
      have h_pre_synth : pre.synthesisPresent = true := by
        rw [h_synth_eq] at h_post_synth; exact h_post_synth
      have h_pre_all : pre.allTerminal := h_inv h_pre_synth
      intro s hs
      rw [h_states] at hs
      exact set_preserves_allTerminal h_t h_pre_all s hs
  | spawn_synthesis h_not_yet h_guard h_states h_synth_set =>
      -- the spawn guard *is* the conclusion (over the unchanged bridge set).
      intro s hs
      rw [h_states] at hs
      exact h_guard s hs

/-- A `Trace` preserves the barrier invariant. -/
theorem Trace.preserves_barrier {g g' : WorkflowGroup}
    (h_inv : g.barrierInvariant) (h_trace : Trace g g') :
    g'.barrierInvariant := by
  induction h_trace with
  | refl => exact h_inv
  | step h_step _ ih =>
      exact ih (h_step.preserves_barrier h_inv)

/-- An `Initial` group satisfies the barrier invariant vacuously: synthesis is
    absent, so the implication holds with a false antecedent. -/
theorem Initial.barrierInvariant {g : WorkflowGroup}
    (h : Initial g) : g.barrierInvariant := by
  intro h_synth
  rw [h.1] at h_synth
  exact absurd h_synth (by decide)

/-- **Barrier-completeness (derived).**

If synthesis is present in a *reachable* fan-out group, every fan-out bridge in
that group is terminal.

This is no longer conclusion-absorbing: the bad state — `synthesisPresent = true`
with some non-terminal bridge — is a representable `WorkflowGroup`, and the proof
*works* to exclude it. It runs by induction over the reachable `Trace`, with the
load-bearing content in `Step.preserves_barrier` (terminalization monotonicity +
the synthesis spawn guard): synthesis can only be set under the all-terminal
guard, and no later terminalization can un-terminalize the group. -/
theorem barrier_completeness {g : WorkflowGroup}
    (r : Reachable g) (h_synth : g.synthesisPresent = true) :
    g.allTerminal := by
  obtain ⟨init, h_init, h_trace⟩ := r
  exact (h_trace.preserves_barrier h_init.barrierInvariant) h_synth

/-- Companion non-vacuity fact (addresses finding lean-2): the barrier is not
    content-free because an `Initial` (hence non-empty, all-running) group is
    *not* all-terminal — so the synthesis flag genuinely gates over a state in
    which `allTerminal` can be false. An `N = 0` group is excluded by `hne` and
    so cannot vacuously satisfy `allTerminal` at the initial state. -/
theorem initial_has_running_witness {g : WorkflowGroup}
    (h : Initial g) : ¬ g.allTerminal :=
  h.not_allTerminal

end Workflow
