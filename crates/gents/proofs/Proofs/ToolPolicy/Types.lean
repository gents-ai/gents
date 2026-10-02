import Proofs.CommandPolicy.ArtifactAuthority
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Lattice.Basic

namespace ToolPolicy

/-- Goal-tool authority must be explicit. Missing state fails closed. -/
def resolveGoalTools : Option Bool → Bool
  | none => false
  | some enabled => enabled

/-- Goal-creation authority must be explicit. Missing state fails closed. -/
def resolveGoalCreate : Option Bool → Bool
  | none => false
  | some enabled => enabled

abbrev ToolId := String

inductive FileCap where
  | off
  | readOnly
  | readWrite
  deriving DecidableEq, Repr

structure ValueMeet (V : Type) where
  vmeet : V → V → V
  vle : V → V → Prop
  vle_refl : ∀ a, vle a a
  vmeet_le_left : ∀ a b, vle (vmeet a b) a
  vmeet_le_right : ∀ a b, vle (vmeet a b) b

inductive EndpointScope (K V : Type) where
  | none
  | only (keys : Finset K) (val : K → V)
  | all

/-- Stable wire discriminator; not an authority ordering. -/
def executionModeContractCode : CommandPolicy.ExecutionMode → Nat
  | .readOnly => 0
  | .workspaceWrite => 1
  | .unrestricted => 2
  | .artifactWrite => 3

structure BashPolicy where
  mode : CommandPolicy.ExecutionMode
  network : CommandPolicy.NetworkMode
  forbidden : Finset (List String)
  allowed : EndpointScope (List String) Unit
  readOnly : EndpointScope String Unit
  sandbox : Bool

structure Surface where
  file : FileCap
  bash : BashPolicy
  goalTools : Bool
  goalCreate : Bool
  defraQuery : Bool
  selfConfig : Bool
  memory : Bool
  schemaManagement : Bool
  /-- Native P2P observations require explicit node-management authority. -/
  p2pRead : Bool
  /-- Mutations also require read authority. Enrollment and DefraDB remain
  the authority owners for peer access and document changes. -/
  p2pMutate : Bool
  sessionHistory : Bool
  contextBudget : Bool
  /-- The agents tool group (`SubagentTools.enabled`): `agent_new` over the
  `subagentTargets` allowlist, `agent_message`, `agent_interrupt` and
  `agent_list`. -/
  sessionMessages : Bool
  skills : Bool
  lsp : Bool
  cliTools : EndpointScope ToolId (Finset String)
  mcpServices : EndpointScope ToolId Unit
  defraCollections : EndpointScope ToolId Unit
  /-- Replication and sync collection grants do not imply generic query access. -/
  p2pCollections : EndpointScope ToolId Unit
  selfConfigCategories : EndpointScope ToolId Unit
  subagentTargets : EndpointScope (String × String) Unit
  backgroundTools : EndpointScope ToolId Unit
  writeTools : EndpointScope (String × String) (Finset String)
  queryTools : EndpointScope (String × String) (Finset String)
  ethQueryMethods : EndpointScope String Unit
  ethCallTools : EndpointScope ToolId Unit
  pluginTools : EndpointScope ToolId Unit

abbrev Avail := Surface

end ToolPolicy
