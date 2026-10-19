import Proofs.InferenceCall.UsageProjection
import Lean

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

private def usageJson (value : Usage) : Json := Json.mkObj
  [("prompt_tokens", toJson value.promptTokens),
   ("completion_tokens", toJson value.completionTokens)]

private def rowJson (entry : Nat × UsageRow) : Json :=
  let row := entry.2
  Json.mkObj
    [("key", toJson entry.1),
     ("call_id", toJson row.persisted.call.callId),
     ("request_physical", toJson row.physicalRequest),
     ("request_logical", toJson row.persisted.call.requestId),
     ("backend", toJson row.persisted.call.backend.val),
     ("state", toJson row.persisted.call.state.toDefraDB),
     ("terminal_stamp", toJson row.persisted.terminalStamp),
     ("usage", row.persisted.usage.map usageJson |>.getD Json.null)]

private def parentJson (value : ParentScope) : Json := Json.mkObj
  [("physical", toJson value.physical), ("logical", toJson value.logical),
   ("node", toJson value.node), ("session", toJson value.session),
   ("state", toJson value.state.toDefraDB)]

private def bindingJson (value : CanonicalOutput.Execution.Handover.AuxiliaryBinding) : Json :=
  Json.mkObj
    [("purpose", toJson value.purpose.toWire),
     ("physical", toJson value.physicalRequest),
     ("logical", toJson value.logicalRequest),
     ("parent_physical", toJson value.parentPhysical),
     ("parent_logical", toJson value.parentLogical),
     ("node", toJson value.node), ("session", toJson value.session),
     ("authenticated", toJson value.authenticated)]

private def bindingFactJson (value : AuxiliaryBindingFact) : Json :=
  Json.mkObj
    [("physical", toJson value.physical),
     ("binding", bindingJson value.binding)]

private def totalsJson (totals : Nat × Nat) : Json := Json.mkObj
  [("prompt_tokens", toJson totals.1),
   ("completion_tokens", toJson totals.2)]

private def resultJson : Except CatalogError Totals → Json
  | .ok totals => Json.mkObj
      [("kind", toJson "ok"),
       ("normal_public", totalsJson totals.normalPublic),
       ("parent_inclusive_audit", totalsJson totals.parentInclusiveAudit)]
  | .error .mismatchedCallId => Json.mkObj
      [("kind", toJson "error"), ("error", toJson "mismatched_call_id")]
  | .error .conflictingCallId => Json.mkObj
      [("kind", toJson "error"), ("error", toJson "conflicting_call_id")]
  | .error .missingPurpose => Json.mkObj
      [("kind", toJson "error"), ("error", toJson "missing_purpose")]
  | .error .missingAuxiliaryBinding => Json.mkObj
      [("kind", toJson "error"), ("error", toJson "missing_title_binding")]
  | .error .invalidAuxiliaryBinding => Json.mkObj
      [("kind", toJson "error"), ("error", toJson "invalid_title_binding")]

private def evaluate (c : Case) (rows : RawRows) : Except CatalogError Totals :=
  validateAndProject rows (purposeLookup c.purposes) (bindingLookup c.bindings) c.parent

private def evaluateBudget (c : Case) (rows : RawRows) : Except BudgetError Nat :=
  recoverBudget rows (purposeLookup c.purposes) (bindingLookup c.bindings) c.parent

private def budgetJson : Except BudgetError Nat → Json
  | .ok used => Json.mkObj [("kind", toJson "ok"), ("used", toJson used)]
  | .error (.catalog error) => resultJson (.error error)
  | .error .unreportedUsage => Json.mkObj
      [("kind", toJson "error"), ("error", toJson "unreported_usage")]

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

private def caseJson (c : Case) : Json := Json.mkObj
  [("name", toJson c.name),
   ("parent", parentJson c.parent),
   ("rows", toJson (c.rows.map rowJson)),
   ("signed_purposes", toJson (c.purposes.map fun fact => Json.mkObj
      [("physical", toJson fact.physical), ("purpose", toJson fact.purpose.toWire)])),
   ("title_bindings", toJson (c.bindings.map bindingFactJson)),
   ("usage_action", c.action.map (fun action => Json.mkObj
      [("call_id", toJson action.callId), ("usage", usageJson action.usage)])
      |>.getD Json.null),
   ("expected_before", resultJson (evaluate c c.rows)),
   ("expected_after", resultJson (evaluate c (applyAction c.rows c.action))),
   ("budget_before", budgetJson (evaluateBudget c c.rows)),
   ("budget_after", budgetJson (evaluateBudget c (applyAction c.rows c.action))),
   ("goal_before", budgetJson (evaluateGoal c c.rows)),
   ("goal_after", budgetJson (evaluateGoal c (applyAction c.rows c.action)))]

def casesJson : String := (toJson (titleUsageCases.map caseJson)).compress

end Conformance.TitleUsage
