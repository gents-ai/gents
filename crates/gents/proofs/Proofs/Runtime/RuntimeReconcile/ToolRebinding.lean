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

/-- What an adopted toolset changed about one tool or agent target, as the
config call's receipt reports it. -/
inductive Change where
  /-- The next turn can call it; the request's toolset did not have it. -/
  | added
  /-- The next turn no longer has it, because the Tools no longer select it. -/
  | removed
  /-- The Tools select it but the adopted toolset leaves it out. -/
  | waiting
  | unchanged
  deriving DecidableEq, Repr

/-- The receipt's account of one tool or agent target: `held` by the request's
toolset before the commit, `selected` by the committed Tools, `offered` by the
adopted toolset. Resolution withholds a local agent target whose agent the
generation does not run (an agent that is disabled, or waiting on a grant), so
a selected target can be missing from a toolset a fresh request would claim.
The receipt names such a target as waiting, with its agent's reason, and never
reports it in effect; a held target that is withheld waits too, since the
Tools still select it. -/
def change (held selected offered : Bool) : Change :=
  if offered then (if held then .unchanged else .added)
  else if selected then .waiting
  else if held then .removed else .unchanged

theorem only_an_offered_item_is_reported_in_effect (held selected offered : Bool)
    (h : change held selected offered = .added) : offered = true := by
  cases held <;> cases selected <;> cases offered <;> simp_all [change]

theorem a_selected_item_the_toolset_leaves_out_waits (held : Bool) :
    change held true false = .waiting := by
  cases held <;> rfl

theorem a_selected_item_is_never_reported_removed (held offered : Bool) :
    change held true offered ≠ .removed := by
  cases held <;> cases offered <;> simp [change]

end RuntimeReconcile.ToolRebinding
