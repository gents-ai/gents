import Proofs.Tools.PeerRegistryDiscovery.RootAdmission

/-!
# The root a Tools save authors

A save that sets `Tools.host.root` under configured WorkspaceRoot policy is
admitted only when the root resolves inside a root the policy publishes; a
freshly authored root gets no overlap tolerance. Preview runs the same
admission and commits nothing. An admitted apply persists the canonical
resolution, never the authored text, so the stored root is the one the
execution boundary re-admits.

The model starts after the host resolver: in a tree without symbolic links the
canonical resolution is the lexical one, which is what `resolve` computes.
Rust owner: `gents_tools::surface::canonicalize_tools_root`, reached through
the self-config tools preview and apply.
-/

namespace SelfConfig.ToolsRoot

open PeerRegistryDiscovery.RootAdmission

inductive Operation where
  | preview
  | apply
  deriving DecidableEq, Repr

/-- Lexical resolution of components under one anchor. -/
def resolve (authored : List String) : List String :=
  (authored.foldl (fun resolved component =>
    if component == ".." then resolved.dropLast else resolved ++ [component]) [])

def anchor : String := "sandbox"

/-- The root an operator selected for the Agent, the only published root. -/
def selected : CanonicalPath := ⟨anchor, ["selected"]⟩

def admittedRoot (authored : List String) : Bool :=
  decide (rootSelectionOk true {selected} false (some ⟨anchor, resolve authored⟩))

structure Outcome where
  admitted : Bool
  committed : Bool
  persisted : Option (List String)
  deriving DecidableEq, Repr

def save (operation : Operation) (authored : List String) : Outcome :=
  let admitted := admittedRoot authored
  let committed := admitted && operation == .apply
  { admitted, committed, persisted := if committed then some (resolve authored) else none }

theorem a_preview_never_commits (authored : List String) :
    (save .preview authored).committed = false := by
  simp only [save]; cases admittedRoot authored <;> decide

theorem direct_tools_preview_and_apply_enforce_and_persist_canonical_workspace_root :
    save .preview ["selected", "detour", ".."] = ⟨true, false, none⟩ ∧
      (save .preview ["sibling"]).admitted = false ∧
      (save .apply ["sibling"]).admitted = false ∧
      save .apply ["selected", "detour", ".."] = ⟨true, true, some ["selected"]⟩ := by
  native_decide

end SelfConfig.ToolsRoot
