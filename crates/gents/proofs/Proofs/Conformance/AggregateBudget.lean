import Proofs.Conformance.ContractCases.AggregateBudget
import Proofs.Conformance.Family

namespace Conformance.AggregateBudget

open Lean PromptAssembly.AggregateBudget

deriving instance ToJson for CallKind, PostChargeAction

inductive ChargeKind where
  | missing
  | within
  | exhausted
  | overrun
  deriving ToJson

def ChargeKind.ofResult : ChargeResult → ChargeKind
  | .missing => .missing
  | .within _ => .within
  | .exhausted _ => .exhausted
  | .overrun _ => .overrun

structure LedgerInput where
  name : String
  limit : Nat
  requestDocId : String
  priorRequestDocIds : List String
  priorCallKinds : List CallKind
  priorPromptTokens : List Nat
  priorCompletionTokens : List Nat
  inputTokens : Nat
  configuredMaxOutputTokens : Nat
  reportedInputTokens : Nat
  reportedOutputTokens : Nat
  reportedTotalTokens : Nat
  usagePresent : Bool
  terminalValid : Bool
  deriving ToJson

def LedgerInput.rows (i : LedgerInput) : List DurableCallUsage :=
  ((i.priorRequestDocIds.zip i.priorCallKinds).zip
    (i.priorPromptTokens.zip i.priorCompletionTokens)).map fun ((request, kind), (prompt, completion)) =>
      { requestDocId := request, kind, usage := { promptTokens := prompt, completionTokens := completion } }

def LedgerInput.usage (i : LedgerInput) : Option Usage :=
  if i.usagePresent then
    some { inputTokens := i.reportedInputTokens, outputTokens := i.reportedOutputTokens
           reportedTotal := i.reportedTotalTokens }
  else none

def LedgerInput.ledger (i : LedgerInput) : Ledger :=
  (rehydrateRequest i.requestDocId (some i.limit) i.rows).getD ⟨i.limit, 0⟩

def LedgerInput.ofCase (c : Conformance.ContractCases.AggregateTokenBudgetCase) : LedgerInput :=
  { name := c.name, limit := c.limit, requestDocId := c.requestDocId
    priorRequestDocIds := c.priorRequestDocIds
    priorCallKinds := c.priorCallKinds.map fun kind =>
      if kind == "compaction" then .compaction else .inference
    priorPromptTokens := c.priorPromptTokens, priorCompletionTokens := c.priorCompletionTokens
    inputTokens := c.inputTokens, configuredMaxOutputTokens := c.configuredMaxOutputTokens
    reportedInputTokens := c.reportedInputTokens, reportedOutputTokens := c.reportedOutputTokens
    reportedTotalTokens := c.reportedTotalTokens, usagePresent := c.usagePresent
    terminalValid := c.terminalValid }

structure LedgerOutput where
  used : Nat
  effectiveOutputTokens : Nat
  canDispatch : Bool
  chargedTokens : Nat
  chargeResult : ChargeKind
  nextUsed : Option Nat
  postChargeAction : PostChargeAction
  deriving ToJson

def ledgerModel (i : LedgerInput) : LedgerOutput :=
  let ledger := i.ledger
  let result := chargeReported ledger i.usage
  { used := ledger.used
    effectiveOutputTokens := effectiveOutputBudget ledger i.inputTokens i.configuredMaxOutputTokens
    canDispatch := decide (CanDispatch ledger i.inputTokens i.configuredMaxOutputTokens)
    chargedTokens := (i.usage.map Usage.chargedTotal).getD 0
    chargeResult := ChargeKind.ofResult result
    nextUsed := (sharedCharge (some ledger) i.usage).map Ledger.used
    postChargeAction := postChargeAction result i.terminalValid }

def requestScoped (i : LedgerInput) : Bool :=
  i.priorRequestDocIds.all (· == i.requestDocId)

/-- The owned loop's ledger rehydrates from usage rows already scoped to its
physical request, so only histories without another request's rows reach it. -/
@[conformance_family]
def ledger : Family LedgerInput LedgerOutput where
  inputs := (Conformance.ContractCases.aggregateTokenBudgetCases.map LedgerInput.ofCase).filter
    requestScoped
  model := ledgerModel

/-- model-production-gap: the owned loop's ledger owner
(`sum_charged_from_persisted_parts`) receives rows the served-node query has
already scoped to the physical request; no pure production path takes a
history that mixes requests, so request scoping is replayed only by the served
rehydration test. -/
@[conformance_family]
def ledgerUnreached : Family LedgerInput LedgerOutput where
  inputs := (Conformance.ContractCases.aggregateTokenBudgetCases.map LedgerInput.ofCase).filter
    (!requestScoped ·)
  model := ledgerModel

structure SharedInput where
  name : String
  limit : Nat
  /-- The ledger the borrowers share, already rehydrated. -/
  used : Nat
  reportedInputTokens : Nat
  reportedOutputTokens : Nat
  reportedTotalTokens : Nat
  usagePresent : Bool
  deriving ToJson

structure SharedOutput where
  nextUsed : Option Nat
  abandonedUsed : Option Nat
  deriving ToJson

@[conformance_family]
def shared : Family SharedInput SharedOutput where
  inputs := Conformance.ContractCases.aggregateTokenBudgetCases.map fun c =>
    { name := c.name, limit := c.limit, used := c.used
      reportedInputTokens := c.reportedInputTokens, reportedOutputTokens := c.reportedOutputTokens
      reportedTotalTokens := c.reportedTotalTokens, usagePresent := c.usagePresent }
  model := fun i =>
    let usage : Option Usage := if i.usagePresent then
      some { inputTokens := i.reportedInputTokens, outputTokens := i.reportedOutputTokens
             reportedTotal := i.reportedTotalTokens }
    else none
    { nextUsed := (sharedCharge (some ⟨i.limit, i.used⟩) usage).map Ledger.used
      abandonedUsed := (sharedCharge (abandonShared (some ⟨i.limit, i.used⟩)) usage).map Ledger.used }

end Conformance.AggregateBudget
