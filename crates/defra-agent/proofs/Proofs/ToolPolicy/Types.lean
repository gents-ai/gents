import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Lattice.Basic

/-!
# Tool Policy Types

Atoms and aggregate surface for unified tool-policy composition.
-/

namespace ToolPolicy

abbrev ToolId := String

inductive FileCap where
  | off
  | readOnly
  | readWrite
  deriving DecidableEq, Repr

/-- A value meet with an explicit lower-bound relation. -/
structure ValueMeet (V : Type) where
  vmeet : V → V → V
  vle : V → V → Prop
  vle_refl : ∀ a, vle a a
  vmeet_le_left : ∀ a b, vle (vmeet a b) a
  vmeet_le_right : ∀ a b, vle (vmeet a b) b

/-- Keyed endpoint scope. `only` is structurally single-valued:
    values are represented by a total function, observed only at `keys`. -/
inductive EndpointScope (K V : Type) where
  | none
  | only (keys : Finset K) (val : K → V)
  | all

inductive ExecMode where
  | readOnly
  | workspaceWrite
  | unrestricted
  deriving DecidableEq, Repr

inductive NetMode where
  | disabled
  | inherit
  | enabled
  deriving DecidableEq, Repr

/-- Bash policy is a product, not a single rank. -/
structure BashPolicy where
  mode : ExecMode
  network : NetMode
  forbidden : Finset (List String)
  allowed : EndpointScope (List String) Unit
  readOnly : EndpointScope String Unit
  sandbox : Bool

/-- Full per-category surface. Used for behavior policy, operator ceiling, and
    runtime availability. -/
structure Surface where
  file : FileCap
  bash : BashPolicy
  meta : Bool
  defraQuery : Bool
  memory : Bool
  sessionHistory : Bool
  contextBudget : Bool
  spawn : Bool
  steering : Bool
  background : Bool
  orchestration : Bool
  crossDeployment : Bool
  skills : Bool
  cliTools : EndpointScope ToolId (Finset String)
  mcpServices : EndpointScope ToolId Unit
  defraCollections : EndpointScope ToolId Unit
  subagentTargets : EndpointScope (String × String) Unit
  backgroundTools : EndpointScope ToolId Unit
  writeTools : EndpointScope (String × String) (Finset String)

abbrev Avail := Surface

end ToolPolicy
