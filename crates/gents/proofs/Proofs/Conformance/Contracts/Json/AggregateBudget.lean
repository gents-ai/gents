import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.AggregateBudget
import Proofs.Conformance.Family

namespace Conformance.BudgetRehydration

open Lean PromptAssembly.AggregateBudget Conformance.ContractCases

deriving instance ToJson for CallKind, Ledger

structure RowWire where
  requestDocId : String
  kind : CallKind
  promptTokens : Nat
  completionTokens : Nat
  deriving ToJson

structure RehydrationInput where
  name : String
  requestDocId : String
  pinnedLimit : Option Nat
  rows : List RowWire
  deriving ToJson

@[conformance_family]
def ledger : Family RehydrationInput (Option Ledger) where
  inputs := budgetRehydrationCases.map fun c =>
    { name := c.name, requestDocId := c.requestDocId, pinnedLimit := c.pinnedLimit
      rows := c.rows.map fun row =>
        { requestDocId := row.requestDocId, kind := row.kind
          promptTokens := row.usage.promptTokens
          completionTokens := row.usage.completionTokens } }
  model := fun i =>
    rehydrateRequest i.requestDocId i.pinnedLimit
      (i.rows.map fun row => ⟨row.requestDocId, row.kind, ⟨row.promptTokens, row.completionTokens⟩⟩)

end Conformance.BudgetRehydration
