import Proofs.Background

/-!
# Workflow.FanOut

Lean model for the `fan_out_and_synthesize` barrier obligation.

The runtime persists one `AgentToolCall` bridge per fan-out child. A workflow
group is the non-empty, bounded set of those bridges sharing one
`workflow_group_id`; the synthesis bridge may be spawned only after every
fan-out bridge is terminal in the parent-visible bridge vocabulary.
-/

namespace Workflow

open Subagent
open ToolExecution

/-- Parent-visible bridge state for a fan-out child.

The completed path is projected by the bridge-complete transition. Every
non-completed terminal goes through `ChildTerminal.projectedToolState`, matching
the runtime bridge-failure projection (`interrupted -> cancelled`, other
failures -> failed). -/
def bridgeToolState (b : BridgedState) : ToolCallState :=
  match b.terminalOf with
  | .running => .running
  | .completed => .completed
  | t => t.projectedToolState

/-- A fan-out group: the fan-out bridges plus the optional synthesis bridge.

`groupId` is the durable `workflow_group_id`, equal to the
`fan_out_and_synthesize` orchestration tool call id in Rust. `hne` rules out
the vacuous `N = 0` barrier; `hwidth` reuses the backgrounded-per-parent cap. -/
structure FanOutGroup where
  groupId : ToolCallId
  bridges : List BridgedState
  synthesis : Option BridgedState
  hne : bridges ≠ []
  hwidth : bridges.length ≤ Subagent.maxBackgroundedPerParent

/-- Every fan-out bridge has reached a terminal bridge state. -/
def allTerminal (g : FanOutGroup) : Prop :=
  ∀ b ∈ g.bridges, isTerminal (bridgeToolState b)

/-- Reachable workflow groups for the cut-1 barrier model.

The only constructor that introduces a synthesis bridge carries `allTerminal`
as its enabling premise. This is the runtime-enforced barrier: the LLM chooses
what to fan out over, but it cannot materialize synthesis before this predicate
holds over durable bridge rows. -/
inductive Reachable : FanOutGroup → Prop where
  | fanout_active (g : FanOutGroup) (h_none : g.synthesis = none) : Reachable g
  | synthesis_spawn
      (base : FanOutGroup)
      (synth : BridgedState)
      (h_none : base.synthesis = none)
      (h_terminal : allTerminal base) :
      Reachable { base with synthesis := some synth }

/-- Barrier-completeness: if synthesis exists in a reachable fan-out group, all
fan-out bridges in that group are terminal. -/
theorem barrier_completeness {g : FanOutGroup} (r : Reachable g) :
    g.synthesis.isSome → allTerminal g := by
  intro h_synthesis
  cases r with
  | fanout_active g h_none =>
      simp [h_none] at h_synthesis
  | synthesis_spawn base synth h_none h_terminal =>
      simpa [allTerminal] using h_terminal

end Workflow
