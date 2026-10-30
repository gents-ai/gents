import Proofs.CommandPolicy.ArtifactAuthority
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Lattice.Basic

namespace ToolPolicy

def resolveGoalTools : Option Bool → Bool
  | none => false
  | some enabled => enabled

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
  /-- Mutations also require read authority. -/
  p2pMutate : Bool
  sessionHistory : Bool
  contextBudget : Bool
  /-- The agents tool group (`AgentTools.enabled`): `agent_new` over the
  `agentTargets` allowlist, `agent_message`, `agent_interrupt` and
  `agent_list`, and the read-only `agent_session_list` and `agent_session_read`.
  Session reads and sends retain the requester and target contract in `DurableLineage`. -/
  sessionMessages : Bool
  skills : Bool
  lsp : Bool
  cliTools : EndpointScope ToolId (Finset String)
  mcpServices : EndpointScope ToolId Unit
  defraCollections : EndpointScope ToolId Unit
  p2pCollections : EndpointScope ToolId Unit
  selfConfigCategories : EndpointScope ToolId Unit
  agentTargets : EndpointScope (String × String) Unit
  backgroundTools : EndpointScope ToolId Unit
  writeTools : EndpointScope (String × String) (Finset String)
  queryTools : EndpointScope (String × String) (Finset String)
  pluginTools : EndpointScope ToolId Unit

abbrev Avail := Surface

end ToolPolicy
