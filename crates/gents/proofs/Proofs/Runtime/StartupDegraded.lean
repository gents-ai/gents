import Lean
import Proofs.Pack.CorePacks

namespace Runtime.StartupDegraded
open Lean

inductive Reason where
  | agentDisabled | backendNotConfigured | backendDisabled
  | backendTemporarilyUnavailable | credentialsRequired
  | runtimeConfigurationInvalid | subscriptionNotEntitled | inferenceProfileInvalid
  | toolConfigurationInvalid | toolSurfaceUnavailable | executorStartFailed
  deriving ToJson, DecidableEq, Repr

/-- Configuration availability may defer an Agent while the node starts.
An unusable tool surface is a failed admission, rather than deferred availability. -/
def allowsStartup : Reason → Bool
  | .agentDisabled | .backendNotConfigured | .backendDisabled
  | .backendTemporarilyUnavailable | .credentialsRequired => true
  | .runtimeConfigurationInvalid | .subscriptionNotEntitled | .inferenceProfileInvalid
  | .toolConfigurationInvalid | .toolSurfaceUnavailable | .executorStartFailed => false

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

/-- How an agent a core pack ships stands in the node's readiness under what
the pack leaves, given whether its Tools run host tools, which read the
Engineer's tool root only with a reader on it: disabled while the pack's
agents are not active, without a tool surface when enabled without that
reader, ready otherwise. -/
def packAgentReason (wiring : Pack.CorePacks.Wiring) (hostTools : Bool) : Option Reason :=
  if !wiring.agentsActive then some .agentDisabled
  else if hostTools && !wiring.rootReaders then some .toolSurfaceUnavailable
  else none

def initAgentReason (corePack : Pack.CorePacks.CorePack) (hostTools : Bool) : Option Reason :=
  packAgentReason (Pack.CorePacks.initWiring corePack) hostTools

def enabledAgentReason (corePack : Pack.CorePacks.CorePack) (hostTools : Bool) :
    Option Reason :=
  packAgentReason (Pack.CorePacks.enabledWiring corePack) hostTools

/-- A node `init` makes is ready: no agent of any core pack it installs
degrades it, whatever tools that agent runs. -/
theorem init_leaves_the_node_ready (hostTools : Bool) :
    nodeReady (Pack.CorePacks.all.filterMap fun pack => initAgentReason pack hostTools) =
      true := by
  cases hostTools <;> decide

/-- One enable makes every agent of the pack ready: it is enabled with the
reader its host tools need, never enabled without it. -/
theorem enabling_activates_with_grants (corePack : Pack.CorePacks.CorePack)
    (hostTools : Bool) : enabledAgentReason corePack hostTools = none := by
  cases corePack <;> cases hostTools <;> rfl

/-- What init leaves of dynamic workflows is off, not broken. -/
theorem not_enabled_pack_agents_are_off (hostTools : Bool) :
    initAgentReason .dynamicWorkflows hostTools = some .agentDisabled := rfl

end Runtime.StartupDegraded
