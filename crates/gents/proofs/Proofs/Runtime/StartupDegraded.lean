import Lean
import Proofs.Pack.CorePacks

namespace Runtime.StartupDegraded
open Lean

inductive Reason where
  | agentDisabled | backendNotConfigured | backendDisabled
  | backendTemporarilyUnavailable | credentialsRequired
  | runtimeConfigurationInvalid | subscriptionNotEntitled | inferenceProfileInvalid
  | toolConfigurationInvalid | toolSurfaceUnavailable | workspaceRootReaderMissing
  | executorStartFailed
  deriving ToJson, DecidableEq, Repr

/-- Configuration availability may defer an Agent while the node starts.
An unusable tool surface, or host tools whose root the Agent cannot read, is a
failed admission, rather than deferred availability. -/
def allowsStartup : Reason → Bool
  | .agentDisabled | .backendNotConfigured | .backendDisabled
  | .backendTemporarilyUnavailable | .credentialsRequired => true
  | .runtimeConfigurationInvalid | .subscriptionNotEntitled | .inferenceProfileInvalid
  | .toolConfigurationInvalid | .toolSurfaceUnavailable | .workspaceRootReaderMissing
  | .executorStartFailed => false

/-- Whether an unavailable Agent makes the node report itself degraded. A
disabled Agent is off by choice, its owner's or a not-enabled pack's
(`Pack.CorePacks.initWiring`): it is not reconciled, has no tool surface to
build and lacks nothing, so the node is ready without it. Every other reason
is something an enabled Agent needs and lacks. -/
def degradesNode : Reason → Bool
  | .agentDisabled => false
  | _ => true

/-- The node's readiness from the reasons its unavailable Agents carry. -/
def nodeReady (unavailable : List Reason) : Bool :=
  unavailable.all fun reason => !degradesNode reason

/-- How the node publishes the root an Agent's host tools read, as the guard
sees it when it decides admission. -/
inductive RootPublishing where
  | noPolicy | publishedEnabled | publishedDisabled
  deriving ToJson, DecidableEq, Repr

/-- What the policy says about the root the Agent's tools read: admitted, denied
because nothing this Agent may use is published, or denied because the path
falls outside every published root. -/
inductive RootDenial where
  | admitted | noAllowedRoots | outside
  deriving ToJson, DecidableEq, Repr

/-- Why the tool surface could not be built, as the runtime reports it. A root
the node publishes and enables over the Agent's tools, which the Agent cannot
read, is the missing reader — the same grant fixes it whether the Agent reads
nothing at all (`noAllowedRoots`) or reads another root (`outside`). Every
other denial is the unadmitted surface, whose fix is a configuration check. -/
def toolSurfaceReason (publishing : RootPublishing) (denial : RootDenial) : Reason :=
  match publishing, denial with
  | .publishedEnabled, .noAllowedRoots => .workspaceRootReaderMissing
  | .publishedEnabled, .outside => .workspaceRootReaderMissing
  | _, _ => .toolSurfaceUnavailable

/-- The cause states a reader grant only when the node publishes and enables a
root over the Agent's tools: the one state a grant on that root changes. -/
theorem reader_cause_needs_an_enabled_published_root (publishing : RootPublishing)
    (denial : RootDenial) :
    toolSurfaceReason publishing denial = .workspaceRootReaderMissing →
      publishing = .publishedEnabled := by
  cases publishing <;> cases denial <;> simp [toolSurfaceReason]

/-- With no policy, or none enabled, the guard's denial is about the path, not
about a reader: the surface stays generically unavailable. -/
theorem no_enabled_root_keeps_the_generic_surface (denial : RootDenial) :
    toolSurfaceReason .publishedDisabled denial = .toolSurfaceUnavailable ∧
      toolSurfaceReason .noPolicy denial = .toolSurfaceUnavailable := by
  cases denial <;> exact ⟨rfl, rfl⟩

/-- How an agent a core pack ships stands under what the pack leaves: disabled
while the pack's agents are not active, ready once enabled. -/
def packAgentReason (wiring : Pack.CorePacks.Wiring) : Option Reason :=
  if !wiring.agentsActive then some .agentDisabled else none

def initAgentReason (corePack : Pack.CorePacks.CorePack) : Option Reason :=
  packAgentReason (Pack.CorePacks.initWiring corePack)

def enabledAgentReason (corePack : Pack.CorePacks.CorePack) : Option Reason :=
  packAgentReason (Pack.CorePacks.enabledWiring corePack)

/-- A node `init` makes is ready: no agent of any core pack it installs
degrades it, whatever tools that agent runs. -/
theorem init_leaves_the_node_ready :
    nodeReady (Pack.CorePacks.all.filterMap fun pack => initAgentReason pack) = true := by
  decide

/-- One enable makes every agent of the pack ready: it is enabled with the
reader its host tools need, never enabled without it. -/
theorem enabling_activates (corePack : Pack.CorePacks.CorePack) :
    enabledAgentReason corePack = none := by
  cases corePack <;> rfl

/-- What init leaves of dynamic workflows is off, not broken. -/
theorem not_enabled_pack_agents_are_off :
    initAgentReason .dynamicWorkflows = some .agentDisabled := rfl

end Runtime.StartupDegraded
