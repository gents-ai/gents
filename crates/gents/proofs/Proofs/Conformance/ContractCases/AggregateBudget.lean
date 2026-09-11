import Proofs.PromptAssembly.AggregateBudget

namespace Conformance.ContractCases

open PromptAssembly.AggregateBudget

structure AggregateTokenBudgetCase where
  name : String
  limit : Nat
  used : Nat
  requestDocId : String
  priorRequestDocIds : List String
  priorCallKinds : List String
  priorPromptTokens : List Nat
  priorCompletionTokens : List Nat
  inputTokens : Nat
  configuredMaxOutputTokens : Nat
  reportedInputTokens : Nat
  reportedOutputTokens : Nat
  reportedTotalTokens : Nat
  usagePresent : Bool
  terminalValid : Bool
  effectiveOutputTokens : Nat
  canDispatch : Bool
  chargedTokens : Nat
  chargeResult : String
  nextUsed : Option Nat
  postChargeAction : String
  deriving Repr

private structure AggregateTokenBudgetWitness where
  name : String
  limit : Nat
  priorUsage : List PersistedUsage
  scopedUsage : Option (List DurableCallUsage) := none
  inputTokens : Nat
  configuredMaxOutputTokens : Nat
  usage : Option Usage
  terminalValid : Bool

private def chargeResultName : ChargeResult → String
  | .missing => "missing"
  | .within _ => "within"
  | .exhausted _ => "exhausted"
  | .overrun _ => "overrun"

private def chargeResultLedger : ChargeResult → Option Ledger
  | .missing => none
  | .within ledger => some ledger
  | .exhausted ledger => some ledger
  | .overrun ledger => some ledger

private def postChargeActionName : PostChargeAction → String
  | .continue => "continue"
  | .succeed => "succeed"
  | .fail => "fail"

private def witnesses : List AggregateTokenBudgetWitness :=
  [ { name := "restart-zero-usage-adds-no-spend"
    , limit := 1000
    , priorUsage := [{ promptTokens := 0, completionTokens := 0 }]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := true
    , usage := some { inputTokens := 100, outputTokens := 50, reportedTotal := 150 } }
  , { name := "restart-mixed-rows-preserve-accounted-spend"
    , limit := 1000
    , priorUsage := [{ promptTokens := 200, completionTokens := 100 },
                     { promptTokens := 0, completionTokens := 0 },
                     { promptTokens := 50, completionTokens := 25 }]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 100, outputTokens := 50, reportedTotal := 150 } }
  , { name := "first-dispatch-clamps-to-total-budget"
    , limit := 1000
    , priorUsage := []
    , inputTokens := 200
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 200, outputTokens := 300, reportedTotal := 500 } }
  , { name := "tool-turn-uses-request-wide-remainder"
    , limit := 1000
    , priorUsage := [{ promptTokens := 500, completionTokens := 0 }]
    , inputTokens := 300
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 300, outputTokens := 199, reportedTotal := 499 } }
  , { name := "retracted-attempt-still-reduces-retry-budget"
    , limit := 1000
    , priorUsage := [{ promptTokens := 500, completionTokens := 0 }]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 100, outputTokens := 400, reportedTotal := 500 } }
  , { name := "exhausted-ledger-blocks-next-dispatch"
    , limit := 1000
    , priorUsage := [{ promptTokens := 1000, completionTokens := 0 }]
    , inputTokens := 1
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 1, outputTokens := 1, reportedTotal := 2 } }
  , { name := "input-consumes-remaining-budget"
    , limit := 1000
    , priorUsage := [{ promptTokens := 700, completionTokens := 0 }]
    , inputTokens := 300
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := none }
  , { name := "missing-usage-fails-closed"
    , limit := 1000
    , priorUsage := [{ promptTokens := 100, completionTokens := 0 }]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := none }
  , { name := "all-zero-usage-fails-closed"
    , limit := 1000
    , priorUsage := [{ promptTokens := 100, completionTokens := 0 }]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 0, outputTokens := 0, reportedTotal := 0 } }
  , { name := "inconsistent-total-uses-durable-components"
    , limit := 1000
    , priorUsage := [{ promptTokens := 100, completionTokens := 0 }]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 300, outputTokens := 200, reportedTotal := 400 } }
  , { name := "larger-reported-total-does-not-rewrite-durable-components"
    , limit := 1000
    , priorUsage := [{ promptTokens := 100, completionTokens := 0 }]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 100, outputTokens := 50, reportedTotal := 200 } }
  , { name := "exact-limit-is-exhausted"
    , limit := 1000
    , priorUsage := [{ promptTokens := 500, completionTokens := 0 }]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := true
    , usage := some { inputTokens := 100, outputTokens := 400, reportedTotal := 500 } }
  , { name := "observed-overrun-fails-closed"
    , limit := 1000
    , priorUsage := [{ promptTokens := 800, completionTokens := 0 }]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 150, outputTokens := 100, reportedTotal := 250 } }
  , { name := "restart-physical-request-includes-compaction-excludes-other-request"
    , limit := 1000
    , priorUsage := []
    , scopedUsage := some
        [ ⟨"physical-a", .inference, ⟨200, 100⟩⟩
        , ⟨"physical-a", .compaction, ⟨50, 25⟩⟩
        , ⟨"physical-b", .inference, ⟨900, 900⟩⟩ ]
    , inputTokens := 100
    , configuredMaxOutputTokens := 900
    , terminalValid := false
    , usage := some { inputTokens := 100, outputTokens := 50, reportedTotal := 150 } }

  ]

private def toCase (witness : AggregateTokenBudgetWitness) : AggregateTokenBudgetCase :=
  let report := witness.usage.getD
    { inputTokens := 0, outputTokens := 0, reportedTotal := 0 }
  let rows := witness.scopedUsage.getD
    (witness.priorUsage.map (fun usage => ⟨"physical-a", .inference, usage⟩))
  let ledger := (rehydrateRequest "physical-a" (some witness.limit) rows).get
    (by simp [rehydrateRequest])
  let result := chargeReported ledger witness.usage
  { name := witness.name
  , limit := ledger.limit
  , used := ledger.used
  , requestDocId := "physical-a"
  , priorRequestDocIds := rows.map DurableCallUsage.requestDocId
  , priorCallKinds := rows.map (fun row => match row.kind with
      | .inference => "inference"
      | .compaction => "compaction")
  , priorPromptTokens := rows.map (fun row => row.usage.promptTokens)
  , priorCompletionTokens := rows.map (fun row => row.usage.completionTokens)
  , inputTokens := witness.inputTokens
  , configuredMaxOutputTokens := witness.configuredMaxOutputTokens
  , reportedInputTokens := report.inputTokens
  , reportedOutputTokens := report.outputTokens
  , reportedTotalTokens := report.reportedTotal
  , usagePresent := witness.usage.isSome
  , terminalValid := witness.terminalValid
  , effectiveOutputTokens := effectiveOutputBudget ledger witness.inputTokens
      witness.configuredMaxOutputTokens
  , canDispatch := decide (CanDispatch ledger witness.inputTokens
      witness.configuredMaxOutputTokens)
  , chargedTokens := report.chargedTotal
  , chargeResult := chargeResultName result
  , nextUsed := (chargeResultLedger result).map Ledger.used
  , postChargeAction := postChargeActionName (postChargeAction result witness.terminalValid) }

def aggregateTokenBudgetCases : List AggregateTokenBudgetCase :=
  witnesses.map toCase

/-- Restart observations distinguish zero usage from no prior rows while
preserving the same accounted spend. Mixed rows charge both durable components. -/
theorem restart_cases_pinned :
    (aggregateTokenBudgetCases.take 2).map
      (fun row => (row.used, row.effectiveOutputTokens, row.nextUsed)) =
      [(0, 900, some 150), (375, 525, some 525)] := by
  rfl

theorem physical_scope_case_pinned :
    aggregateTokenBudgetCases.getLast?.map
      (fun row => (row.used, row.effectiveOutputTokens, row.nextUsed)) =
      some (375, 525, some 525) := by rfl

/-- Decoded execution-owner facts, not caller overrides. Zero is an exhausted
runtime ledger, never an alias for unlimited (authored zero limits are rejected
by configuration validation before this boundary). -/
structure BudgetRehydrationCase where
  name : String
  requestDocId : String
  pinnedLimit : Option Nat
  rows : List DurableCallUsage

def BudgetRehydrationCase.result (c : BudgetRehydrationCase) : Option Ledger :=
  rehydrateRequest c.requestDocId c.pinnedLimit c.rows

private def mixedRequestUsage : List DurableCallUsage :=
  [⟨"physical-a", .inference, ⟨200, 100⟩⟩,
   ⟨"physical-a", .compaction, ⟨50, 25⟩⟩,
   ⟨"physical-b", .inference, ⟨900, 900⟩⟩]

def budgetRehydrationCases : List BudgetRehydrationCase :=
  [ ⟨"unlimited-with-no-usage", "physical-a", none, []⟩
  , ⟨"unlimited-remains-unlimited-with-durable-usage", "physical-a", none, mixedRequestUsage⟩
  , ⟨"zero-pinned-runtime-limit-is-not-unlimited", "physical-a", some 0, mixedRequestUsage⟩
  , ⟨"pinned-limit-rehydrates-inference-and-compaction-exact-scope", "physical-a", some 1000, mixedRequestUsage⟩
  , ⟨"other-physical-request-has-its-own-spend", "physical-b", some 2000, mixedRequestUsage⟩ ]

theorem budget_rehydration_cases_pinned : budgetRehydrationCases.map (·.result) =
    [none, none, some ⟨0, 375⟩, some ⟨1000, 375⟩, some ⟨2000, 1800⟩] := by decide

end Conformance.ContractCases
