import Proofs.InferenceCall.UsageProjection
import Lean

/-!
# Title usage projection cases

Inputs serialize physical inference-call rows, signed physical request purposes,
and authenticated typed title provenance. Expected totals and catalog errors
are evaluated through `InferenceCall.UsageProjection.validateAndProject`.
Late usage actions use the existing `InferenceCall.Persistence.observeUsage`.
-/

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

private structure TitleBindingFact where
  physical : CanonicalOutput.DocId
  binding : TitleBinding

private structure UsageAction where
  callId : Nat
  usage : Usage

private structure Case where
  name : String
  parent : ParentScope
  rows : RawRows
  purposes : List SignedPurpose
  bindings : List TitleBindingFact
  action : Option UsageAction := none

private def parent : ParentScope :=
  { physical := 10, logical := 20, agent := 30, session := 40, state := .completed }

private def mkRow (callId physical logical : Nat) (usage : Option Usage) : Nat × UsageRow :=
  (callId,
    { physicalRequest := physical
    , persisted :=
        { call :=
            { callId := callId
            , requestId := logical
            , backend := { val := "backend" }
            , state := .completed }
        , terminalStamp := some 99
        , usage := usage } })

private def usage : Usage := { promptTokens := 3, completionTokens := 5 }
private def otherUsage : Usage := { promptTokens := 4, completionTokens := 5 }

private def binding : CanonicalOutput.Execution.Handover.TitleBinding :=
  { physicalRequest := 11, logicalRequest := 21
  , parentPhysical := 10, parentLogical := 20
  , agent := 30, session := 40, authenticated := true }

private def bindingFact (b : TitleBinding) : TitleBindingFact :=
  { physical := 11, binding := b }

private def titlePurpose : SignedPurpose := ⟨11, .titleAudit⟩
private def normalPurpose : SignedPurpose := ⟨10, .normal⟩

private def titleUsageCases : List Case := [
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
    purposes := [titlePurpose], bindings := [bindingFact { binding with agent := 35 }] },
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

private def bindingLookup (facts : List TitleBindingFact)
    (physical : CanonicalOutput.DocId) : Option TitleBinding :=
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
   ("agent", toJson value.agent), ("session", toJson value.session),
   ("state", toJson value.state.toDefraDB)]

private def bindingJson (value : CanonicalOutput.Execution.Handover.TitleBinding) : Json :=
  Json.mkObj
    [("physical", toJson value.physicalRequest),
     ("logical", toJson value.logicalRequest),
     ("parent_physical", toJson value.parentPhysical),
     ("parent_logical", toJson value.parentLogical),
     ("agent", toJson value.agent), ("session", toJson value.session),
     ("authenticated", toJson value.authenticated)]

private def bindingFactJson (value : TitleBindingFact) : Json :=
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
  | .error .missingTitleBinding => Json.mkObj
      [("kind", toJson "error"), ("error", toJson "missing_title_binding")]
  | .error .invalidTitleBinding => Json.mkObj
      [("kind", toJson "error"), ("error", toJson "invalid_title_binding")]

private def evaluate (c : Case) (rows : RawRows) : Except CatalogError Totals :=
  validateAndProject rows (purposeLookup c.purposes) (bindingLookup c.bindings) c.parent

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
      some (.error .invalidTitleBinding, .error .invalidTitleBinding) ∧
    evaluateNamed "same_physical_wrong_logical" =
      some (.error .invalidTitleBinding, .error .invalidTitleBinding) := by
  native_decide

theorem unrelated_parent_is_excluded_not_invalid :
    evaluateNamed "wrong_parent_physical" =
      some (.ok ⟨(0, 0), (0, 0)⟩, .ok ⟨(0, 0), (0, 0)⟩) := by
  native_decide

theorem unauthenticated_binding_is_invalid :
    evaluateNamed "unauthenticated_title_binding" =
      some (.error .invalidTitleBinding, .error .invalidTitleBinding) := by
  native_decide

theorem missing_dependencies_do_not_become_zero_totals :
    evaluateNamed "missing_signed_purpose" =
      some (.error .missingPurpose, .error .missingPurpose) ∧
    evaluateNamed "missing_authenticated_title_binding" =
      some (.error .missingTitleBinding, .error .missingTitleBinding) := by
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
   ("expected_after", resultJson (evaluate c (applyAction c.rows c.action)))]

def casesJson : String := (toJson (titleUsageCases.map caseJson)).compress

end Conformance.TitleUsage
