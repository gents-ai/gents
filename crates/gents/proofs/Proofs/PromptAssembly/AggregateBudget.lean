import Proofs.Basic

/-! The provider tokenizer and truthfulness of provider usage reports are
external assumptions. The durable call owner must ensure completed calls have
reported usage before restart accounting omits unreported rows. -/

namespace PromptAssembly.AggregateBudget

structure Usage where
  inputTokens : Nat
  outputTokens : Nat
  reportedTotal : Nat
  deriving DecidableEq, Repr

/-- `reportedTotal` remains an observed provider fact, not a second accounting
source with no corresponding durable column. -/
def Usage.chargedTotal (usage : Usage) : Nat :=
  usage.inputTokens + usage.outputTokens

/-- Crash boundary: rehydration sees these durable columns, not the transient
provider response. Production admits a row only when both durable components
are present; wholly absent rows are omitted. PersistedUsage models accounted
rows, not call completion. -/
structure PersistedUsage where
  promptTokens : Nat
  completionTokens : Nat
  deriving DecidableEq, Repr

def Usage.persisted (usage : Usage) : PersistedUsage :=
  { promptTokens := usage.inputTokens
  , completionTokens := usage.outputTokens }

def PersistedUsage.chargedTotal (usage : PersistedUsage) : Nat :=
  usage.promptTokens + usage.completionTokens

def rehydrateUsed (rows : List PersistedUsage) : Nat :=
  (rows.map PersistedUsage.chargedTotal).sum

structure Ledger where
  limit : Nat
  used : Nat
  deriving DecidableEq, Repr

/-- Accounting uses the exact physical request; logical retry labels, goals and
sessions must not widen the query. -/
inductive CallKind where
  | inference | compaction
  deriving DecidableEq, Repr

structure DurableCallUsage where
  requestDocId : String
  kind : CallKind
  usage : PersistedUsage
  deriving DecidableEq, Repr

def requestUsage (requestDocId : String) (rows : List DurableCallUsage) : List PersistedUsage :=
  (rows.filter (fun row => row.requestDocId == requestDocId)).map DurableCallUsage.usage

/-- The limit is pinned by the execution owner from InferenceExecution once,
never supplied by request input; restart does not reread edited configuration.
None is unlimited, distinct from zero and from the per-call output ceiling. -/
def rehydrateRequest (requestDocId : String) (pinnedLimit : Option Nat)
    (rows : List DurableCallUsage) : Option Ledger :=
  pinnedLimit.map (fun limit => ⟨limit, rehydrateUsed (requestUsage requestDocId rows)⟩)

theorem other_request_usage_excluded (requestDocId : String) (row : DurableCallUsage)
    (rows : List DurableCallUsage) (h : row.requestDocId ≠ requestDocId) :
    requestUsage requestDocId (row :: rows) = requestUsage requestDocId rows := by
  simp [requestUsage, h]

theorem rehydrate_keeps_pinned_limit (requestDocId : String) (limit : Nat)
    (rows : List DurableCallUsage) :
    (rehydrateRequest requestDocId (some limit) rows).map Ledger.limit = some limit := by rfl

theorem no_limit_stays_unlimited (requestDocId : String) (rows : List DurableCallUsage) :
    rehydrateRequest requestDocId none rows = none := by rfl

theorem owned_call_counts (requestDocId : String) (kind : CallKind)
    (usage : PersistedUsage) (rows : List DurableCallUsage) :
    rehydrateUsed (requestUsage requestDocId (⟨requestDocId, kind, usage⟩ :: rows)) =
      usage.chargedTotal + rehydrateUsed (requestUsage requestDocId rows) := by
  simp [requestUsage, rehydrateUsed]

def Ledger.remaining (ledger : Ledger) : Nat :=
  ledger.limit - ledger.used

def effectiveOutputBudget
    (ledger : Ledger) (inputTokens configuredMaxOutputTokens : Nat) : Nat :=
  min configuredMaxOutputTokens (ledger.remaining - inputTokens)

def CanDispatch
    (ledger : Ledger) (inputTokens configuredMaxOutputTokens : Nat) : Prop :=
  0 < effectiveOutputBudget ledger inputTokens configuredMaxOutputTokens

instance (ledger : Ledger) (inputTokens configuredMaxOutputTokens : Nat) :
    Decidable (CanDispatch ledger inputTokens configuredMaxOutputTokens) := by
  unfold CanDispatch
  infer_instance

def Ledger.charge (ledger : Ledger) (usage : Usage) : Ledger :=
  { ledger with used := ledger.used + usage.chargedTotal }

inductive ChargeResult where
  | missing
  | within (ledger : Ledger)
  | exhausted (ledger : Ledger)
  | overrun (ledger : Ledger)
  deriving DecidableEq, Repr

inductive PostChargeAction where
  | continue
  | succeed
  | fail
  deriving DecidableEq, Repr

def postChargeAction (result : ChargeResult) (terminalValid : Bool) : PostChargeAction :=
  match result with
  | .missing | .overrun _ => .fail
  | .within _ => if terminalValid then .succeed else .continue
  | .exhausted _ => if terminalValid then .succeed else .fail

/-- A missing or all-zero provider usage report is not enforceable. -/
def chargeReported (ledger : Ledger) (usage : Option Usage) : ChargeResult :=
  match usage with
  | none => .missing
  | some report =>
      if report.chargedTotal = 0 then
        .missing
      else
        let next := ledger.charge report
        if next.used > next.limit then
          .overrun next
        else if next.used = next.limit then
          .exhausted next
        else
          .within next

theorem charged_total_eq_persisted_components (usage : Usage) :
    usage.chargedTotal = usage.inputTokens + usage.outputTokens := by
  rfl

theorem persist_preserves_charge (usage : Usage) :
    usage.persisted.chargedTotal = usage.chargedTotal := by
  rfl

/-- Production must await this durable append before publishing the terminal
stream item. -/
theorem rehydrate_after_persist (rows : List PersistedUsage) (usage : Usage) :
    rehydrateUsed (rows ++ [usage.persisted]) =
      rehydrateUsed rows + usage.chargedTotal := by
  induction rows with
  | nil =>
      simp [rehydrateUsed, Usage.persisted, PersistedUsage.chargedTotal,
        Usage.chargedTotal]
  | cons head tail ih =>
      change head.chargedTotal + rehydrateUsed (tail ++ [usage.persisted]) =
        head.chargedTotal + rehydrateUsed tail + usage.chargedTotal
      rw [ih]
      omega

theorem charge_monotone (ledger : Ledger) (usage : Usage) :
    ledger.used ≤ (ledger.charge usage).used := by
  simp [Ledger.charge]

/-- If the ledger starts within its limit, the dispatch clamp keeps the
estimated input plus requested output within the unspent request budget. -/
theorem dispatch_respects_remaining
    {ledger : Ledger} {inputTokens configuredMaxOutputTokens : Nat}
    (hwithin : ledger.used ≤ ledger.limit)
    (hdispatch : CanDispatch ledger inputTokens configuredMaxOutputTokens) :
    ledger.used + inputTokens +
      effectiveOutputBudget ledger inputTokens configuredMaxOutputTokens ≤
        ledger.limit := by
  have hinput : inputTokens ≤ ledger.remaining := by
    unfold CanDispatch effectiveOutputBudget at hdispatch
    omega
  unfold effectiveOutputBudget Ledger.remaining at hinput ⊢
  omega

theorem exhausted_cannot_dispatch
    {ledger : Ledger} {inputTokens configuredMaxOutputTokens : Nat}
    (hexhausted : ledger.limit ≤ ledger.used) :
    ¬ CanDispatch ledger inputTokens configuredMaxOutputTokens := by
  unfold CanDispatch effectiveOutputBudget Ledger.remaining
  omega

theorem exhausted_succeeds_iff_terminal_valid
    (ledger : Ledger) (terminalValid : Bool) :
    postChargeAction (.exhausted ledger) terminalValid = .succeed ↔
      terminalValid = true := by
  cases terminalValid <;> simp [postChargeAction]

theorem missing_usage_fails (terminalValid : Bool) :
    postChargeAction .missing terminalValid = .fail := by
  simp [postChargeAction]

theorem two_charges_are_additive
    (ledger : Ledger) (first second : Usage) :
    ((ledger.charge first).charge second).used =
      ledger.used + first.chargedTotal + second.chargedTotal := by
  rfl

end PromptAssembly.AggregateBudget
