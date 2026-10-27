import Proofs.ToolPolicy.Meet
import Proofs.SelfConfig.Cases

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

/-! ## Job-guard selection rows -/

/-- A self-config write of a Tools `integrations` text: `stored = none` is a
Tools document without the field. -/
structure JobGuardRow where
  name : String
  operator : Bool
  selectedBefore : Bool
  stored : Option FieldValue
  candidate : FieldValue

/-- A Tools document with self-config enabled and the given `integrations`. -/
def jobGuardToolsDoc (integrations : Option FieldValue) : Doc :=
  Doc.ofList (("self_config", "{\"enable_self_config\":true}") ::
    integrations.toList.map (fun v => ("integrations", v)))

/-- Operator writes do not pass through the guard (`keepsGrants`); a model
write is judged by `keepsGrants` against the previously selected Tools, or by
`reselectionKeepsGrants` when nothing was selected before. -/
def JobGuardRow.accepted (r : JobGuardRow) : Bool :=
  if r.operator then true
  else if r.selectedBefore then
    keepsGrants ContractCases.decodeGrants Grants.bot
      (jobGuardToolsDoc r.stored) (jobGuardToolsDoc (some r.candidate))
  else
    reselectionKeepsGrants ContractCases.decodeGrants Grants.bot none
      (some (jobGuardToolsDoc (some r.candidate)))

def jobGuardRows : List JobGuardRow :=
  let J := ContractCases.jobIntegrations
  [ { name := "model_job_ceiling_narrowing", operator := false, selectedBefore := true
    , stored := some (J 8 "check"), candidate := J 4 "check" }
  , { name := "model_job_ceiling_widening", operator := false, selectedBefore := true
    , stored := some (J 4 "check"), candidate := J 8 "check" }
  , { name := "model_job_policy_edit", operator := false, selectedBefore := true
    , stored := some (J 4 "check"), candidate := J 4 "off" }
  , { name := "model_job_block_added", operator := false, selectedBefore := true
    , stored := none, candidate := J 4 "check" }
  , { name := "model_reselection_from_none_acquires_job", operator := false
    , selectedBefore := false, stored := none, candidate := J 4 "check" }
  , { name := "operator_job_ceiling_widening", operator := true, selectedBefore := true
    , stored := some (J 4 "check"), candidate := J 8 "check" } ]

end SelfConfig
