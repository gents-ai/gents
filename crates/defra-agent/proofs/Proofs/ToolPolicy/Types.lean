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

/-- Bash policy is a product, not a single rank.

    SP1-Rust wiring carve-out: the production `ToolPolicyBash`
    (`tool_surface/policy.rs`) currently mirrors `mode`, `network`, `allowed`
    (as `allowed_argv_prefixes`), and `sandbox`. The `forbidden` (union-meet) and
    `readOnly` (intersection-meet) factors are MODELED and PROVEN here
    (`bash_meet_forbidden_superset`, `bash_meet_readonly_*` in `Theorems.lean`)
    but are NOT yet threaded through the operator-ceiling meet on the Rust side:
    a behavior's own forbidden/read-only prefixes are still enforced via the
    legacy `CommandExecutionPolicy` passthrough (`toolset/shared/command.rs`), so
    there is no runtime soundness gap — only a ceiling-narrowing capability the
    Rust resolver does not yet expose. Closing it is an explicit SP1-Rust item
    (add both factors to `ToolPolicyBash` + the conformance bash view), tracked
    alongside the bash sub-field → executable-validator projection. Until then the
    conformance bash cases hold `forbidden := ∅` / `readOnly := .all`, so the
    proven bounds are not yet exercised end-to-end. -/
structure BashPolicy where
  mode : ExecMode
  network : NetMode
  forbidden : Finset (List String)
  allowed : EndpointScope (List String) Unit
  readOnly : EndpointScope String Unit
  sandbox : Bool

/-- Full per-category surface. Used for behavior policy, operator ceiling, and
    runtime availability.

    SCOPE / category-completeness carve-out: this Surface models the
    DOCUMENT-DRIVEN tool categories — every tool that a `ToolSelection` document
    can configure and that the operator ceiling therefore governs. It deliberately
    does NOT model `custom_tools` (`tool_surface/mod.rs` `CustomToolFactory`):
    those are CODE-INJECTED at runtime-construction time, not configured by any
    document, and so live at a higher trust boundary (whoever links the binary)
    than the document control plane. They are an intentional out-of-band extension
    point, not an escape hatch in this model. SP1-Rust must NOT silently treat them
    as ceiling-governed; if code-injected tools ever need to be capped by the
    document ceiling, that is a separate, explicit decision (add a `custom` field
    here first). "Category-complete" = complete over document-driven categories. -/
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
