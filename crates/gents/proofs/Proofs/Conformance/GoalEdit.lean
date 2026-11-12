import Proofs.Automation.GoalAutomation.Edit
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Functions.Automation
import Proofs.Conformance.Family

namespace GoalAutomation.Edit
deriving instance Lean.ToJson for Assignment, Fields, Origin
end GoalAutomation.Edit

namespace Conformance.GoalEditContracts

open Lean GoalAutomation.Edit Conformance.Contracts

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

structure EditInput where
  name : String
  origin : Origin
  observed : Assignment
  current : Assignment
  fields : Fields
  deriving ToJson

@[conformance_family]
def edit : Family EditInput (Option Assignment) where
  inputs := cases.map fun (origin, c) =>
    { name := (if origin == .agent then "agent_" else "client_") ++ c.name, origin,
      observed := c.observed, current := c.current, fields := c.fields }
  model := fun i => apply? i.observed i.current i.fields i.origin

/-- The last Unicode scalar value. -/
def lastCodepoint : Nat := 0x10FFFF

structure WhitespaceRun where
  first : Nat
  last : Nat
  deriving ToJson

/-- The code space split into each whitespace scalar value on its own and the
content runs between them. -/
def whitespaceRuns : List WhitespaceRun :=
  let (runs, next) := TextNormalization.objectiveWhitespaceCodepoints.foldl
    (fun (runs, next) codepoint =>
      (runs ++ (if next < codepoint then [⟨next, codepoint - 1⟩] else []) ++
        [⟨codepoint, codepoint⟩], codepoint + 1))
    ([], 0)
  runs ++ (if next ≤ lastCodepoint then [⟨next, lastCodepoint⟩] else [])

@[conformance_family]
def objectiveWhitespaceRuns : Family WhitespaceRun Bool where
  inputs := whitespaceRuns
  model := fun run => TextNormalization.objectiveWhitespace (Char.ofNat run.first)

example : (whitespaceRuns.flatMap fun run =>
    if TextNormalization.objectiveWhitespace (Char.ofNat run.first) then
      (List.range (run.last - run.first + 1)).map (run.first + ·) else []) =
    TextNormalization.objectiveWhitespaceCodepoints := by native_decide

end Conformance.GoalEditContracts
