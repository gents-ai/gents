import Proofs.GoalAutomation.Edit
import Proofs.Conformance.ContractTypes

namespace Conformance.GoalEditContracts

open GoalAutomation.Edit Conformance.Contracts

structure Case where
  name : String
  observed : Assignment
  current : Assignment
  fields : Fields
  deriving Repr

def base : Assignment := ⟨.active, 2, "ship", some 100⟩

private def commonCases : List Case :=
  [ ⟨"objective", base, base, ⟨some "verify", none⟩⟩
  , ⟨"trimmed_objective", base, base, ⟨some "  verify  ", none⟩⟩
  , ⟨"unicode_trimmed_objective", base, base, ⟨some "\u3000verify\u00A0", none⟩⟩
  , ⟨"unicode_blank_objective", base, base, ⟨some "\u3000\u00A0\u0085\u000B\u000C", none⟩⟩
  , ⟨"unicode_exact_retry", base, base, ⟨some "\u00A0ship\u3000", none⟩⟩
  , ⟨"zero_width_space_is_content", base, base, ⟨some "\u200Bverify\u200B", none⟩⟩
  , ⟨"budget_increase", base, base, ⟨none, some 200⟩⟩
  , ⟨"budget_decrease", base, base, ⟨none, some 50⟩⟩
  , ⟨"both_fields", base, base, ⟨some "verify", some 200⟩⟩
  , ⟨"exact_retry", base, base, ⟨some "  ship  ", some 100⟩⟩
  , ⟨"budget_retry", base, base, ⟨none, some 100⟩⟩
  , ⟨"missing_fields", base, base, ⟨none, none⟩⟩
  , ⟨"empty_objective", base, base, ⟨some "", none⟩⟩
  , ⟨"blank_objective", base, base, ⟨some "   ", none⟩⟩
  , ⟨"zero_budget", base, base, ⟨none, some 0⟩⟩
  , ⟨"negative_budget", base, base, ⟨none, some (-1)⟩⟩
  , ⟨"maximum_budget", base, base, ⟨none, some GoalAutomation.maxTokenBudget⟩⟩
  , ⟨"stale_epoch", base, {base with sequence := 3}, ⟨some "verify", none⟩⟩
  , ⟨"stale_observation", {base with status := .paused}, base, ⟨some "verify", none⟩⟩
  , ⟨"paused_objective", {base with status := .paused}, {base with status := .paused},
      ⟨some "verify", none⟩⟩
  , ⟨"paused_budget", {base with status := .paused}, {base with status := .paused},
      ⟨none, some 200⟩⟩
  , ⟨"paused_both_fields", {base with status := .paused}, {base with status := .paused},
      ⟨some "verify", some 9007199254740993⟩⟩
  , ⟨"paused_empty_fields", {base with status := .paused}, {base with status := .paused},
      ⟨none, none⟩⟩
  , ⟨"paused_stale_epoch", {base with status := .paused},
      {base with status := .paused, sequence := 3}, ⟨some "verify", none⟩⟩
  , ⟨"paused_exact_retry", {base with status := .paused}, {base with status := .paused},
      ⟨some "ship", some 100⟩⟩
  , ⟨"complete_observation", {base with status := .complete}, {base with status := .complete},
      ⟨some "verify", none⟩⟩
  , ⟨"stopped_observation", {base with status := .stopped}, {base with status := .stopped},
      ⟨some "verify", none⟩⟩
  ] ++ ([Goals.Status.paused, .blocked, .usageLimited, .budgetLimited, .complete, .stopped].map fun status =>
    ⟨"inactive_" ++ status.toDefraDB, base, {base with status}, ⟨some "verify", none⟩⟩)

def cases : List (Origin × Case) :=
  [Origin.agent, .client].flatMap fun origin => commonCases.map (origin, ·)

example : cases.length = 66 := by native_decide

example : apply? base base ⟨some "verify", none⟩ = some {base with objective := "verify"} := by native_decide
example : apply? base base ⟨some "  ship  ", some 100⟩ = some base := by native_decide
example : apply? base {base with status := .complete} ⟨some "verify", none⟩ = none := by native_decide
example : apply? {base with status := .paused} {base with status := .paused}
    ⟨some "verify", none⟩ .client =
    some {base with status := .paused, objective := "verify"} := by native_decide
example : apply? {base with status := .paused} {base with status := .paused}
    ⟨some "verify", none⟩ .agent = none := by native_decide
example : apply? base {base with status := .paused} ⟨some "verify", none⟩ .client = none := by
  native_decide

private def optionalIntJson : Option Int → String
  | none => "null"
  | some value => toString value

def assignmentJson (assignment : Assignment) : String :=
  "{\"status\":" ++ jsonString assignment.status.toDefraDB ++
  ",\"sequence\":" ++ toString assignment.sequence ++
  ",\"objective\":" ++ jsonString assignment.objective ++
  ",\"budget\":" ++ optionalIntJson assignment.budget ++ "}"

def caseJson (origin : Origin) (c : Case) : String :=
  let source := if origin == .agent then "agent" else "client"
  "{\"name\":" ++ jsonString (source ++ "_" ++ c.name) ++
  ",\"origin\":" ++ jsonString source ++
  ",\"observed\":" ++ assignmentJson c.observed ++
  ",\"current\":" ++ assignmentJson c.current ++
  ",\"fields\":{\"objective\":" ++ (match c.fields.objective with
    | none => "null" | some objective => jsonString objective) ++
  ",\"budget\":" ++ optionalIntJson c.fields.budget ++ "}" ++
  ",\"expected\":" ++ (match apply? c.observed c.current c.fields origin with
    | none => "null" | some assignment => assignmentJson assignment) ++ "}"

def casesJson : String := jsonArray (cases.map fun (origin, c) => caseJson origin c)

def whitespaceCodepointsJson : String :=
  jsonArray (GoalAutomation.objectiveWhitespaceCodepoints.map toString)

end Conformance.GoalEditContracts
