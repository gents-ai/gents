import Proofs.Automation.RuntimeReconcile.State

/-! When a committed change to a running request's own Tools reaches that
request. A request's tools are built once from the generation that claimed it
(`RuntimeState.requestGeneration`); a change to its Tools document is resolved
by the control watcher into a later generation, which a fresh request would
claim. A request whose own config call committed that change rebinds to the
later generation's tools from its next turn, and only to a generation a fresh
request could claim. -/

namespace RuntimeReconcile.ToolRebinding

/-- What the active generation holds for the agent, observed by the config
call that committed a change to the agent's selected Tools document. -/
inductive Publication where
  /-- The active generation is the one the request's tools were built from,
  or older. -/
  | older
  /-- A newer generation is active and does not run the agent. -/
  | unavailable
  /-- A newer generation is active and runs the agent. -/
  | runs
  deriving DecidableEq, Repr

inductive Answer where
  /-- Build the generation's tools and send them from the request's next turn. -/
  | rebind
  /-- Observe the next generation the control watcher activates. -/
  | wait
  /-- The request keeps its tools; the change applies from the next request. -/
  | keep
  deriving DecidableEq, Repr

/-- The config call's answer once it has committed a change to its own agent's
selected Tools document. A generation that runs the agent is adopted exactly
when a fresh request would claim it (`RuntimeState.configurationAdmission`):
every document of its selection is visible to the agent and at the version
the generation resolved. A generation resolved before the commit has an older
Tools version, so it is never adopted, and the call waits for the next one.
A generation without the agent means the committed Tools cannot run; the
request keeps the tools it was claimed with, which were admitted. Waiting
ends at the call's bound, and the request then keeps its tools.

External premise: the control watcher resolves every committed control
change into a newer generation (`ControlDebounce.due`), so a generation that
runs the agent from its current documents eventually exists unless a later
change supersedes it. -/
def answer (publication : Publication) (visible versionsCurrent expired : Bool) : Answer :=
  match publication with
  | .unavailable => .keep
  | .runs =>
      match RuntimeState.configurationAdmission visible versionsCurrent with
      | .claim => .rebind
      | .awaitNewerGeneration => if expired then .keep else .wait
  | .older => if expired then .keep else .wait

theorem rebinds_only_what_a_fresh_request_claims
    (publication : Publication) (visible versionsCurrent expired : Bool) :
    answer publication visible versionsCurrent expired = .rebind ↔
      publication = .runs ∧
        RuntimeState.configurationAdmission visible versionsCurrent = .claim := by
  cases publication <;> cases visible <;> cases versionsCurrent <;> cases expired <;>
    simp [answer, RuntimeState.configurationAdmission]

theorem a_generation_resolved_before_the_commit_is_never_adopted
    (publication : Publication) (visible expired : Bool) :
    answer publication visible false expired ≠ .rebind := by
  cases publication <;> cases visible <;> cases expired <;>
    simp [answer, RuntimeState.configurationAdmission]

theorem the_wait_ends_at_its_bound
    (publication : Publication) (visible versionsCurrent : Bool) :
    answer publication visible versionsCurrent true ≠ .wait := by
  cases publication <;> cases visible <;> cases versionsCurrent <;>
    simp [answer, RuntimeState.configurationAdmission]

theorem an_unrunnable_change_keeps_the_admitted_tools
    (visible versionsCurrent expired : Bool) :
    answer .unavailable visible versionsCurrent expired = .keep := rfl

end RuntimeReconcile.ToolRebinding
