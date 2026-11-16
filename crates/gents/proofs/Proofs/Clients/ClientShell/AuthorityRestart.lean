/-!
# Managed runtime access change

The desktop's "Change access" review for the OS-managed local node
(`LocalServer`, `authorityRestartPlan` in the desktop's
`lib/managedRuntimeAuthority.ts`). A restart applies the reviewed tool ceiling
and root. The restarted runtime admits only a published root
(`PeerRegistryDiscovery.RootAdmission`), so a reviewed root that differs from
the runtime-confirmed one is published for the node's Engineer through the
running runtime first. Publishing needs the node and an agent to publish for;
without either, the change is refused before any write and the refusal names
its cause and the actions that lead forward.
-/

namespace ClientShell.AuthorityRestart

/-- What the review observes when the User confirms it.

`rootChanged`: the reviewed root is set and differs from the runtime-confirmed
root. A cleared root (metatools only) needs no publication. `nodeObserved`:
the desktop has read the local node's agents. `agentPresent`: that node has an
agent to publish the folder for (the Engineer, else the default agent, else
any agent). -/
structure Observation where
  rootChanged : Bool
  nodeObserved : Bool
  agentPresent : Bool
  deriving DecidableEq, Repr

/-- Why a changed folder cannot be published. -/
inductive Refusal where
  /-- The desktop has not read the local node's agents yet. -/
  | nodeUnobserved
  /-- The local node has no agent to publish the folder for. -/
  | noAgent
  deriving DecidableEq, Repr

/-- An action the refusal offers. -/
inductive NextAction where
  /-- Review again with the runtime-confirmed folder, which needs no
  publication. -/
  | keepCurrentFolder
  /-- Confirm the review again once the node's agents are shown. -/
  | retryWhenLoaded
  /-- Add an agent on the node, then confirm the review again. -/
  | addAgent
  deriving DecidableEq, Repr

inductive Plan where
  | restart
  | publishThenRestart
  | refused (cause : Refusal)
  deriving DecidableEq, Repr

def plan (o : Observation) : Plan :=
  if !o.rootChanged then .restart
  else if !o.nodeObserved then .refused .nodeUnobserved
  else if !o.agentPresent then .refused .noAgent
  else .publishThenRestart

/-- Decision: keeping the current folder is always offered first, because it
is the one action that needs nothing else from the node. -/
def nextActions : Refusal → List NextAction
  | .nodeUnobserved => [.keepCurrentFolder, .retryWhenLoaded]
  | .noAgent => [.keepCurrentFolder, .addAgent]

def allObservations : List Observation :=
  [false, true].flatMap fun rootChanged =>
    [false, true].flatMap fun nodeObserved =>
      [false, true].map fun agentPresent => ⟨rootChanged, nodeObserved, agentPresent⟩

/-- A changed folder never restarts without being published first. -/
theorem changed_root_never_restarts_unpublished (o : Observation)
    (h : o.rootChanged = true) : plan o ≠ .restart := by
  obtain ⟨r, n, a⟩ := o
  subst h
  cases n <;> cases a <;> simp [plan]

theorem publishes_only_a_changed_root (o : Observation)
    (h : plan o = .publishThenRestart) : o.rootChanged = true := by
  obtain ⟨r, n, a⟩ := o
  cases r <;> cases n <;> cases a <;> simp_all [plan]

/-- Every refusal names at least one next action. -/
theorem refusal_names_next_action (cause : Refusal) : nextActions cause ≠ [] := by
  cases cause <;> simp [nextActions]

/-- Every refusal offers keeping the current folder, and from any observation
that choice leads to a restart. -/
theorem keep_current_folder_restarts (o : Observation) (cause : Refusal) :
    .keepCurrentFolder ∈ nextActions cause ∧
      plan { o with rootChanged := false } = .restart := by
  cases cause <;> simp [nextActions, plan]

end ClientShell.AuthorityRestart
