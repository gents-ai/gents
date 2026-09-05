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

inductive ExecMode where
  | readOnly
  | workspaceWrite
  | artifactWrite
  | unrestricted
  deriving DecidableEq, Repr

def ExecMode.toCommand : ExecMode → CommandPolicy.ExecutionMode
  | .readOnly => .readOnly
  | .workspaceWrite => .workspaceWrite
  | .artifactWrite => .artifactWrite
  | .unrestricted => .unrestricted

def ExecMode.fromCommand : CommandPolicy.ExecutionMode → ExecMode
  | .readOnly => .readOnly
  | .workspaceWrite => .workspaceWrite
  | .artifactWrite => .artifactWrite
  | .unrestricted => .unrestricted

def ExecMode.meet (a b : ExecMode) : ExecMode :=
  fromCommand (a.toCommand.meet b.toCommand)

theorem ExecMode.meet_toCommand (a b : ExecMode) :
    (a.meet b).toCommand = a.toCommand.meet b.toCommand := by
  cases a <;> cases b <;> decide

@[simp] theorem ExecMode.meet_idem (a : ExecMode) : a.meet a = a := by
  cases a <;> decide

/-- Stable wire discriminator; not an authority ordering. -/
def ExecMode.toContractCode : ExecMode → Nat
  | .readOnly => 0
  | .workspaceWrite => 1
  | .unrestricted => 2
  | .artifactWrite => 3

inductive NetMode where
  | disabled
  | inherit
  | enabled
  deriving DecidableEq, Repr

structure BashPolicy where
  mode : ExecMode
  network : NetMode
  forbidden : Finset (List String)
  allowed : EndpointScope (List String) Unit
  readOnly : EndpointScope String Unit
  sandbox : Bool

structure Surface where
  file : FileCap
  bash : BashPolicy
  meta : Bool
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
  crossDeployment : Bool
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
