import Proofs.ToolPolicy.Meet

/-!
# Selection obligations for the agent sibling-tools operation

Every selection here is operation scoped. The write stays an ordinary
self-config Tools write through `SelfConfig.step` under the `keepsControl`
no-lockout guard: nothing here is a parallel write path or an ACP bypass, and
the narrowing guarantee is claimed for this operation only.
-/

namespace SelfConfig

def selectedToolFlag (requested : Option Bool) (existing : Bool) : Bool :=
  requested.getD existing

theorem omitted_tool_selection_preserves (existing : Bool) :
    selectedToolFlag none existing = existing := rfl

theorem explicit_tool_selection_wins (requested existing : Bool) :
    selectedToolFlag (some requested) existing = requested := rfl

/-! ## Network selection -/

/-- The sibling-tools operation may preserve the stored selection or
explicitly narrow it to disabled; inherit/enabled are never admitted inputs. -/
def selectedNetworkMode
    (requested : Option CommandPolicy.NetworkMode)
    (existing : CommandPolicy.NetworkMode) : CommandPolicy.NetworkMode :=
  requested.getD existing

def networkSelectionAllowed : Option CommandPolicy.NetworkMode → Bool
  | none => true
  | some .disabled => true
  | some .inherit | some .enabled => false

theorem omitted_network_selection_preserves (existing : CommandPolicy.NetworkMode) :
    selectedNetworkMode none existing = existing := rfl

theorem only_disabled_network_selection_admitted
    (requested : CommandPolicy.NetworkMode) :
    networkSelectionAllowed (some requested) = true ↔ requested = .disabled := by
  cases requested <;> simp [networkSelectionAllowed]

/-! ## The scoped sibling-tools operation -/

structure SiblingToolsOperation where
  ownerMatches : Bool
  isProtected : Bool
  sharedContext : Bool
  sharedTools : Bool
  requestedNetwork : Option CommandPolicy.NetworkMode
  existingNetwork : CommandPolicy.NetworkMode

/-- The focused operation refuses protected or shared targets rather than
mutating other agents' documents or inventing another materialization owner. -/
def siblingToolsAllowed (ownerMatches isProtected sharedContext sharedTools : Bool) : Bool :=
  ownerMatches && !isProtected && !sharedContext && !sharedTools

def SiblingToolsOperation.admitted (op : SiblingToolsOperation) : Bool :=
  siblingToolsAllowed op.ownerMatches op.isProtected op.sharedContext op.sharedTools &&
    networkSelectionAllowed op.requestedNetwork

def SiblingToolsOperation.resultNetwork (op : SiblingToolsOperation) :
    CommandPolicy.NetworkMode :=
  selectedNetworkMode op.requestedNetwork op.existingNetwork

theorem protected_sibling_tools_denied (owner sharedContext sharedTools : Bool) :
    siblingToolsAllowed owner true sharedContext sharedTools = false := by
  cases owner <;> simp [siblingToolsAllowed]

theorem foreign_owner_sibling_tools_denied (isProtected sharedContext sharedTools : Bool) :
    siblingToolsAllowed false isProtected sharedContext sharedTools = false := by
  simp [siblingToolsAllowed]

theorem shared_context_sibling_tools_denied (owner isProtected sharedTools : Bool) :
    siblingToolsAllowed owner isProtected true sharedTools = false := by
  cases owner <;> simp [siblingToolsAllowed]

theorem shared_tools_sibling_tools_denied (owner isProtected sharedContext : Bool) :
    siblingToolsAllowed owner isProtected sharedContext true = false := by
  cases owner <;> simp [siblingToolsAllowed]

theorem unshared_owned_sibling_tools_allowed :
    siblingToolsAllowed true false false false = true := rfl

theorem sibling_tools_operation_narrows_network (op : SiblingToolsOperation)
    (h : op.admitted = true) :
    ToolPolicy.networkRank op.resultNetwork ≤ ToolPolicy.networkRank op.existingNetwork := by
  cases hop : op.requestedNetwork with
  | none => simp [SiblingToolsOperation.resultNetwork, selectedNetworkMode, hop]
  | some requested =>
      have hallowed : networkSelectionAllowed (some requested) = true := by
        simp only [SiblingToolsOperation.admitted, hop, Bool.and_eq_true] at h
        exact h.2
      rw [(only_disabled_network_selection_admitted requested).mp hallowed] at hop
      simp [SiblingToolsOperation.resultNetwork, selectedNetworkMode, hop,
        ToolPolicy.networkRank]

/-! ## Graph tool presentation -/

/-- Native graph tool presentation is an independent opt-in: presentation is
not graph caller admission and does not grant installation or configuration. -/
def graphToolPresented (requested : Bool) (_selfConfig _packInstall : Bool) : Bool :=
  requested

theorem graph_tools_without_configuration :
    graphToolPresented true false false = true := rfl

theorem configuration_does_not_grant_graph_tools :
    graphToolPresented false true true = false := rfl

end SelfConfig
