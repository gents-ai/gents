import Proofs.Runtime.InferenceCall.UsageProjection
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session
import Proofs.Inference.ActivityViews

namespace Conformance.TitleUsage

open Lean InferenceCall.UsageProjection

local instance {ε α : Type} [DecidableEq ε] [DecidableEq α] :
    DecidableEq (Except ε α)
  | .error a, .error b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.error.inj e))
  | .error _, .ok _ => .isFalse nofun
  | .ok _, .error _ => .isFalse nofun
  | .ok a, .ok b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.ok.inj e))

private abbrev RawRows := List (Nat × UsageRow)

private structure SignedPurpose where
  physical : CanonicalOutput.DocId
  purpose : RequestPurpose

private structure AuxiliaryBindingFact where
  physical : CanonicalOutput.DocId
  binding : AuxiliaryBinding

private structure UsageAction where
  callId : Nat
  usage : Usage

private structure Case where
  name : String
  parent : ParentScope
  rows : RawRows
  purposes : List SignedPurpose
  bindings : List AuxiliaryBindingFact
  action : Option UsageAction := none

private def parent : ParentScope :=
  { physical := 10, logical := 20, node := 30, session := 40, state := .completed }

private def mkRow (callId physical logical : Nat) (usage : Option Usage)
    (state : InferenceCallState := .completed) : Nat × UsageRow :=
  (callId,
    { physicalRequest := physical
    , persisted :=
        { call :=
            { callId := callId
            , requestId := logical
            , backend := { val := "backend" }
            , state }
        , terminalStamp := some 99
        , usage := usage } })

private def usage : Usage := { promptTokens := 3, completionTokens := 5 }
private def otherUsage : Usage := { promptTokens := 4, completionTokens := 5 }

private def binding : CanonicalOutput.Execution.Handover.AuxiliaryBinding :=
  { physicalRequest := 11, logicalRequest := 21
  , parentPhysical := 10, parentLogical := 20
  , node := 30, session := 40, authenticated := true }

private def bindingFact (b : AuxiliaryBinding) : AuxiliaryBindingFact :=
  { physical := 11, binding := b }

private def titlePurpose : SignedPurpose := ⟨11, .titleAudit⟩
private def normalPurpose : SignedPurpose := ⟨10, .normal⟩

private def titleUsageCases : List Case := [
  { name := "normal_review_and_title_have_separate_goal_charges", parent := parent,
    rows := [mkRow 1 10 20 (some usage), mkRow 7 11 21 (some usage),
      mkRow 8 12 22 (some usage)],
    purposes := [normalPurpose, ⟨11, .accessReview⟩, ⟨12, .titleAudit⟩],
    bindings := [bindingFact { binding with purpose := .accessReview },
      ⟨12, { binding with physicalRequest := 12, logicalRequest := 22 }⟩] },
  { name := "duplicate_review_call_has_one_goal_charge", parent := parent,
    rows := [mkRow 7 11 21 (some usage), mkRow 7 11 21 (some usage)],
    purposes := [⟨11, .accessReview⟩],
    bindings := [bindingFact { binding with purpose := .accessReview }] },
  { name := "empty_budget_has_no_spend", parent := parent,
    rows := [], purposes := [normalPurpose], bindings := [] },
  { name := "access_review_missing_usage_blocks_budget_restart", parent := parent,
    rows := [mkRow 7 11 21 none], purposes := [⟨11, .accessReview⟩],
    bindings := [bindingFact { binding with purpose := .accessReview }] },
  { name := "access_review_zero_usage_blocks_budget_restart", parent := parent,
    rows := [mkRow 7 11 21 (some ⟨0, 0⟩)], purposes := [⟨11, .accessReview⟩],
    bindings := [bindingFact { binding with purpose := .accessReview }] },
  { name := "access_review_late_usage_restores_budget_observation", parent := parent,
    rows := [mkRow 7 11 21 none .cancelled], purposes := [⟨11, .accessReview⟩],
    bindings := [bindingFact { binding with purpose := .accessReview }],
    action := some ⟨7, usage⟩ },
  { name := "failed_access_review_usage_still_reduces_budget", parent := parent,
    rows := [mkRow 7 11 21 (some usage) .failed], purposes := [⟨11, .accessReview⟩],
    bindings := [bindingFact { binding with purpose := .accessReview }] },
  { name := "normal_missing_usage_blocks_budget_restart", parent := parent,
    rows := [mkRow 1 10 20 none], purposes := [normalPurpose], bindings := [] },
  { name := "access_review_usage_is_parent_inclusive_audit_only", parent := parent,
    rows := [mkRow 7 11 21 (some usage)], purposes := [⟨11, .accessReview⟩],
    bindings := [bindingFact { binding with purpose := .accessReview }] },
  { name := "access_review_cannot_replay_title_binding", parent := parent,
    rows := [mkRow 7 11 21 (some usage)], purposes := [⟨11, .accessReview⟩],
    bindings := [bindingFact binding] },
  { name := "title_cannot_replay_access_review_binding", parent := parent,
    rows := [mkRow 7 11 21 (some usage)], purposes := [titlePurpose],
    bindings := [bindingFact { binding with purpose := .accessReview }] },
  { name := "access_review_missing_binding_fails_closed", parent := parent,
    rows := [mkRow 7 11 21 (some usage)], purposes := [⟨11, .accessReview⟩], bindings := [] },
  { name := "normal_public_usage", parent := parent,
    rows := [mkRow 1 10 20 (some usage)], purposes := [normalPurpose], bindings := [] },
  { name := "title_audit_only", parent := parent,
    rows := [mkRow 7 11 21 (some usage)], purposes := [titlePurpose], bindings := [bindingFact binding] },
  { name := "normal_plus_title_parent_inclusive", parent := parent,
    rows := [mkRow 1 10 20 (some usage), mkRow 7 11 21 (some usage)],
    purposes := [normalPurpose, titlePurpose], bindings := [bindingFact binding] },
  { name := "title_late_usage_after_parent_terminal", parent := parent,
    rows := [mkRow 7 11 21 none], purposes := [titlePurpose], bindings := [bindingFact binding],
    action := some ⟨7, usage⟩ },
  { name := "exact_duplicate_call_counts_once", parent := parent,
    rows := [mkRow 7 11 21 (some usage), mkRow 7 11 21 (some usage)],
    purposes := [titlePurpose], bindings := [bindingFact binding] },
  { name := "conflicting_call_id_fails_closed", parent := parent,
    rows := [mkRow 7 11 21 (some usage), mkRow 7 11 21 (some otherUsage)],
    purposes := [titlePurpose], bindings := [bindingFact binding] },
  { name := "mismatched_call_id_fails_closed", parent := parent,
    rows := [(8, (mkRow 7 11 21 (some usage)).2)],
    purposes := [titlePurpose], bindings := [bindingFact binding] },
  { name := "same_logical_wrong_physical", parent := parent,
    rows := [mkRow 7 12 21 (some usage)],
    purposes := [⟨12, .titleAudit⟩], bindings := [⟨12, binding⟩] },
  { name := "same_physical_wrong_logical", parent := parent,
    rows := [mkRow 7 11 22 (some usage)],
    purposes := [titlePurpose], bindings := [bindingFact binding] },
  { name := "wrong_parent_physical", parent := parent,
    rows := [mkRow 7 11 21 (some usage)],
    purposes := [titlePurpose], bindings := [bindingFact { binding with parentPhysical := 15 }] },
  { name := "wrong_parent_logical", parent := parent,
    rows := [mkRow 7 11 21 (some usage)],
    purposes := [titlePurpose], bindings := [bindingFact { binding with parentLogical := 25 }] },
  { name := "wrong_parent_agent", parent := parent,
    rows := [mkRow 7 11 21 (some usage)],
    purposes := [titlePurpose], bindings := [bindingFact { binding with node := 35 }] },
  { name := "wrong_parent_session", parent := parent,
    rows := [mkRow 7 11 21 (some usage)],
    purposes := [titlePurpose], bindings := [bindingFact { binding with session := 45 }] },
  { name := "unauthenticated_title_binding", parent := parent,
    rows := [mkRow 7 11 21 (some usage)],
    purposes := [titlePurpose], bindings := [bindingFact { binding with authenticated := false }] },
  { name := "missing_signed_purpose", parent := parent,
    rows := [mkRow 7 11 21 (some usage)], purposes := [], bindings := [bindingFact binding] },
  { name := "missing_authenticated_title_binding", parent := parent,
    rows := [mkRow 7 11 21 (some usage)], purposes := [titlePurpose], bindings := [] }
]

private def purposeLookup (facts : List SignedPurpose)
    (physical : CanonicalOutput.DocId) : Option RequestPurpose :=
  (facts.find? (fun fact => fact.physical == physical)).map (·.purpose)

private def bindingLookup (facts : List AuxiliaryBindingFact)
    (physical : CanonicalOutput.DocId) : Option AuxiliaryBinding :=
  (facts.find? (fun fact => fact.physical == physical)).map (·.binding)

private def applyAction (rows : RawRows) : Option UsageAction → RawRows
  | none => rows
  | some action => rows.map fun (callId, row) =>
      if callId == action.callId then
        (callId, { row with persisted :=
          InferenceCall.Persistence.observeUsage row.persisted action.usage })
      else (callId, row)

private def evaluate (c : Case) (rows : RawRows) : Except CatalogError Totals :=
  validateAndProject rows (purposeLookup c.purposes) (bindingLookup c.bindings) c.parent

private def evaluateBudget (c : Case) (rows : RawRows) : Except BudgetError Nat :=
  recoverBudget rows (purposeLookup c.purposes) (bindingLookup c.bindings) c.parent

private def evaluateGoal (c : Case) (rows : RawRows) : Except BudgetError Nat :=
  (projectGoal rows (purposeLookup c.purposes) (bindingLookup c.bindings) c.parent).mapError .catalog

theorem goal_budget_counts_review_and_excludes_title :
    (titleUsageCases.find? (fun c => c.name ==
      "normal_review_and_title_have_separate_goal_charges")).map
        (fun c => evaluateGoal c c.rows) = some (.ok 16) ∧
    (titleUsageCases.find? (fun c => c.name ==
      "duplicate_review_call_has_one_goal_charge")).map
        (fun c => evaluateGoal c c.rows) = some (.ok 8) := by
  native_decide

private def evaluateNamedBudget (name : String) :
    Option (Except BudgetError Nat × Except BudgetError Nat) :=
  (titleUsageCases.find? (fun c => c.name == name)).map fun c =>
    (evaluateBudget c c.rows, evaluateBudget c (applyAction c.rows c.action))

theorem review_usage_cannot_become_a_fresh_allowance :
    evaluateNamedBudget "access_review_usage_is_parent_inclusive_audit_only" =
      some (.ok 8, .ok 8) ∧
    evaluateNamedBudget "access_review_missing_usage_blocks_budget_restart" =
      some (.error .unreportedUsage, .error .unreportedUsage) ∧
    evaluateNamedBudget "access_review_zero_usage_blocks_budget_restart" =
      some (.error .unreportedUsage, .error .unreportedUsage) := by
  native_decide

theorem late_report_restores_the_observed_allowance :
    evaluateNamedBudget "access_review_late_usage_restores_budget_observation" =
      some (.error .unreportedUsage, .ok 8) := by
  native_decide

private def evaluateNamed (name : String) :
    Option (Except CatalogError Totals × Except CatalogError Totals) :=
  (titleUsageCases.find? (fun c => c.name == name)).map fun c =>
    (evaluate c c.rows, evaluate c (applyAction c.rows c.action))

theorem title_is_audit_only_witness :
    evaluateNamed "title_audit_only" =
      some (.ok ⟨(0, 0), (3, 5)⟩, .ok ⟨(0, 0), (3, 5)⟩) := by
  native_decide

theorem combined_normal_and_title_witness :
    evaluateNamed "normal_plus_title_parent_inclusive" =
      some (.ok ⟨(3, 5), (6, 10)⟩, .ok ⟨(3, 5), (6, 10)⟩) := by
  native_decide

theorem terminal_parent_late_usage_witness :
    evaluateNamed "title_late_usage_after_parent_terminal" =
      some (.ok ⟨(0, 0), (0, 0)⟩, .ok ⟨(0, 0), (3, 5)⟩) := by
  native_decide

theorem exact_duplicate_and_conflict_witnesses :
    evaluateNamed "exact_duplicate_call_counts_once" =
      some (.ok ⟨(0, 0), (3, 5)⟩, .ok ⟨(0, 0), (3, 5)⟩) ∧
    evaluateNamed "conflicting_call_id_fails_closed" =
      some (.error .conflictingCallId, .error .conflictingCallId) := by
  native_decide

theorem wrong_physical_and_logical_witnesses :
    evaluateNamed "same_logical_wrong_physical" =
      some (.error .invalidAuxiliaryBinding, .error .invalidAuxiliaryBinding) ∧
    evaluateNamed "same_physical_wrong_logical" =
      some (.error .invalidAuxiliaryBinding, .error .invalidAuxiliaryBinding) := by
  native_decide

theorem unrelated_parent_is_excluded_not_invalid :
    evaluateNamed "wrong_parent_physical" =
      some (.ok ⟨(0, 0), (0, 0)⟩, .ok ⟨(0, 0), (0, 0)⟩) := by
  native_decide

theorem unauthenticated_binding_is_invalid :
    evaluateNamed "unauthenticated_title_binding" =
      some (.error .invalidAuxiliaryBinding, .error .invalidAuxiliaryBinding) := by
  native_decide

theorem missing_dependencies_do_not_become_zero_totals :
    evaluateNamed "missing_signed_purpose" =
      some (.error .missingPurpose, .error .missingPurpose) ∧
    evaluateNamed "missing_authenticated_title_binding" =
      some (.error .missingAuxiliaryBinding, .error .missingAuxiliaryBinding) := by
  native_decide

deriving instance ToJson for PromptAssembly.AggregateBudget.PersistedUsage

structure RowWire where
  key : Nat
  callId : Nat
  requestPhysical : Nat
  requestLogical : Nat
  backend : String
  state : InferenceCallState
  terminalStamp : Option Nat
  usage : Option Usage
  deriving ToJson

structure ParentWire where
  physical : Nat
  logical : Nat
  node : Nat
  session : Nat
  state : RequestState
  deriving ToJson

structure PurposeWire where
  physical : Nat
  purpose : String
  deriving ToJson

structure BindingWire where
  purpose : String
  physical : Nat
  logical : Nat
  parentPhysical : Nat
  parentLogical : Nat
  node : Nat
  session : Nat
  authenticated : Bool
  deriving ToJson

structure BindingFactWire where
  physical : Nat
  binding : BindingWire
  deriving ToJson

structure ActionWire where
  callId : Nat
  usage : Usage
  deriving ToJson

structure UsageInput where
  name : String
  parent : ParentWire
  rows : List RowWire
  signedPurposes : List PurposeWire
  titleBindings : List BindingFactWire
  usageAction : Option ActionWire
  deriving ToJson

structure TokenPair where
  promptTokens : Nat
  completionTokens : Nat
  deriving ToJson

structure TotalsResult where
  kind : String
  normalPublic : Option TokenPair
  parentInclusiveAudit : Option TokenPair
  error : Option String
  deriving ToJson

structure BudgetResult where
  kind : String
  used : Option Nat
  error : Option String
  deriving ToJson

structure CatalogOutput where
  expectedBefore : TotalsResult
  expectedAfter : TotalsResult
  deriving ToJson

structure BudgetOutput where
  budgetBefore : BudgetResult
  budgetAfter : BudgetResult
  deriving ToJson

structure GoalOutput where
  goalBefore : BudgetResult
  goalAfter : BudgetResult
  deriving ToJson

private def rowWire (entry : Nat × UsageRow) : RowWire :=
  let row := entry.2
  { key := entry.1, callId := row.persisted.call.callId
    requestPhysical := row.physicalRequest, requestLogical := row.persisted.call.requestId
    backend := row.persisted.call.backend.val, state := row.persisted.call.state
    terminalStamp := row.persisted.terminalStamp, usage := row.persisted.usage }

private def bindingWire (b : AuxiliaryBinding) : BindingWire :=
  { purpose := b.purpose.toWire, physical := b.physicalRequest, logical := b.logicalRequest
    parentPhysical := b.parentPhysical, parentLogical := b.parentLogical
    node := b.node, session := b.session, authenticated := b.authenticated }

private def inputOf (c : Case) : UsageInput :=
  { name := c.name
    parent := { physical := c.parent.physical, logical := c.parent.logical
                node := c.parent.node, session := c.parent.session, state := c.parent.state }
    rows := c.rows.map rowWire
    signedPurposes := c.purposes.map fun f => { physical := f.physical, purpose := f.purpose.toWire }
    titleBindings := c.bindings.map fun f => { physical := f.physical, binding := bindingWire f.binding }
    usageAction := c.action.map fun a => { callId := a.callId, usage := a.usage } }

private def purposeOf (wire : String) : RequestPurpose :=
  (RequestPurpose.fromWire? wire).getD .normal

private def caseOf (i : UsageInput) : Case :=
  { name := i.name
    parent := { physical := i.parent.physical, logical := i.parent.logical
                node := i.parent.node, session := i.parent.session, state := i.parent.state }
    rows := i.rows.map fun r =>
      (r.key, { physicalRequest := r.requestPhysical
              , persisted :=
                  { call := { callId := r.callId, requestId := r.requestLogical
                            , backend := { val := r.backend }, state := r.state }
                  , terminalStamp := r.terminalStamp, usage := r.usage } })
    purposes := i.signedPurposes.map fun f => ⟨f.physical, purposeOf f.purpose⟩
    bindings := i.titleBindings.map fun f =>
      ⟨f.physical, { purpose := purposeOf f.binding.purpose, physicalRequest := f.binding.physical
                   , logicalRequest := f.binding.logical, parentPhysical := f.binding.parentPhysical
                   , parentLogical := f.binding.parentLogical, node := f.binding.node
                   , session := f.binding.session, authenticated := f.binding.authenticated }⟩
    action := i.usageAction.map fun a => ⟨a.callId, a.usage⟩ }

private def catalogErrorWire : CatalogError → String
  | .mismatchedCallId => "mismatched_call_id"
  | .conflictingCallId => "conflicting_call_id"
  | .missingPurpose => "missing_purpose"
  | .missingAuxiliaryBinding => "missing_title_binding"
  | .invalidAuxiliaryBinding => "invalid_title_binding"

private def pairOf (totals : Nat × Nat) : TokenPair :=
  { promptTokens := totals.1, completionTokens := totals.2 }

private def totalsResult : Except CatalogError Totals → TotalsResult
  | .ok t => { kind := "ok", normalPublic := some (pairOf t.normalPublic)
               parentInclusiveAudit := some (pairOf t.parentInclusiveAudit), error := none }
  | .error e => { kind := "error", normalPublic := none, parentInclusiveAudit := none
                  error := some (catalogErrorWire e) }

private def budgetResult : Except BudgetError Nat → BudgetResult
  | .ok used => { kind := "ok", used := some used, error := none }
  | .error (.catalog e) => { kind := "error", used := none, error := some (catalogErrorWire e) }
  | .error .unreportedUsage => { kind := "error", used := none, error := some "unreported_usage" }

private def catalogModel (i : UsageInput) : CatalogOutput :=
  let c := caseOf i
  { expectedBefore := totalsResult (evaluate c c.rows)
    expectedAfter := totalsResult (evaluate c (applyAction c.rows c.action)) }

private def budgetModel (i : UsageInput) : BudgetOutput :=
  let c := caseOf i
  { budgetBefore := budgetResult (evaluateBudget c c.rows)
    budgetAfter := budgetResult (evaluateBudget c (applyAction c.rows c.action)) }

private def goalModel (i : UsageInput) : GoalOutput :=
  let c := caseOf i
  { goalBefore := budgetResult (evaluateGoal c c.rows)
    goalAfter := budgetResult (evaluateGoal c (applyAction c.rows c.action)) }

/-- Signed purposes and auxiliary bindings reach the native catalog
projections already admitted: `verify_historical_auxiliary_receipt` admits them
from signed `AgentRequest` rows on a served node. These cases carry only
bindings that admission accepts, so the projections see exactly what the model
validates. -/
private def admittedBindingCases : List String :=
  [ "empty_budget_has_no_spend", "normal_public_usage", "title_audit_only"
  , "normal_plus_title_parent_inclusive", "access_review_usage_is_parent_inclusive_audit_only"
  , "normal_review_and_title_have_separate_goal_charges"
  , "duplicate_review_call_has_one_goal_charge"
  , "access_review_late_usage_restores_budget_observation"
  , "failed_access_review_usage_still_reduces_budget"
  , "normal_missing_usage_blocks_budget_restart"
  , "access_review_missing_usage_blocks_budget_restart"
  , "access_review_zero_usage_blocks_budget_restart"
  , "title_late_usage_after_parent_terminal", "exact_duplicate_call_counts_once"
  , "conflicting_call_id_fails_closed" ]

private def reached : List UsageInput :=
  (titleUsageCases.filter fun c => c.name ∈ admittedBindingCases).map inputOf

private def unreached : List UsageInput :=
  (titleUsageCases.filter fun c => c.name ∉ admittedBindingCases).map inputOf

/-- The goal usage query selects only normal and access-review requests, so
title-audit rows never reach the goal projection: a catalog whose only
conflict is between title-audit rows is not a goal input. -/
private def titleRowConflictCases : List String := ["conflicting_call_id_fails_closed"]

private def goalReached : List UsageInput :=
  (titleUsageCases.filter fun c =>
    c.name ∈ admittedBindingCases ∧ c.name ∉ titleRowConflictCases).map inputOf

private def goalUnreachedCases : List UsageInput :=
  (titleUsageCases.filter fun c =>
    c.name ∉ admittedBindingCases ∨ c.name ∈ titleRowConflictCases).map inputOf

@[conformance_family]
def catalog : Family UsageInput CatalogOutput where
  inputs := reached
  model := catalogModel

@[conformance_family]
def budget : Family UsageInput BudgetOutput where
  inputs := reached
  model := budgetModel

@[conformance_family]
def goal : Family UsageInput GoalOutput where
  inputs := goalReached
  model := goalModel

/-- model-production-gap: purpose and binding admission (missing or
unauthenticated bindings, a binding replayed across purposes or parents, a
mismatched call key) happens in `verify_historical_auxiliary_receipt` over
signed rows on a served node; the pure catalog projections never see these
inputs. -/
@[conformance_family]
def catalogUnreached : Family UsageInput CatalogOutput where
  inputs := unreached
  model := catalogModel

/-- model-production-gap: as `catalogUnreached`, for budget recovery. -/
@[conformance_family]
def budgetUnreached : Family UsageInput BudgetOutput where
  inputs := unreached
  model := budgetModel

/-- model-production-gap: as `catalogUnreached`, for the goal charge; and the
goal query never loads title-audit rows, while `projectGoal` validates the whole
catalog, so a conflict confined to title-audit rows fails the model's goal
charge but cannot reach production's. -/
@[conformance_family]
def goalUnreached : Family UsageInput GoalOutput where
  inputs := goalUnreachedCases
  model := goalModel

end Conformance.TitleUsage
