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
  sessionHistory : Bool
  contextBudget : Bool
  spawn : Bool
  steering : Bool
  background : Bool
  crossPrincipal : Bool
  skills : Bool
  lsp : Bool
  cliTools : EndpointScope ToolId (Finset String)
  mcpServices : EndpointScope ToolId Unit
  defraCollections : EndpointScope ToolId Unit
  selfConfigCategories : EndpointScope ToolId Unit
  subagentTargets : EndpointScope (String × String) Unit
  backgroundTools : EndpointScope ToolId Unit
  writeTools : EndpointScope (String × String) (Finset String)
  queryTools : EndpointScope (String × String) (Finset String)
  ethQueryMethods : EndpointScope String Unit
  ethCallTools : EndpointScope ToolId Unit

abbrev Avail := Surface

end ToolPolicy
