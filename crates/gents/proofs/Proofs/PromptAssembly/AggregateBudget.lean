import Proofs.Basic

/-!
# Request-wide provider token budget

One durable agent request may issue several provider calls: tool turns and
completed attempts which are later retracted all consume provider tokens. This
model gives those calls one monotone ledger. The runtime must constrain every
dispatch from the ledger and charge every non-zero provider usage report.

The provider tokenizer and the truthfulness of its usage report remain external
assumptions. Production charges the input/output components it persists on the
durable `InferenceCall`, so restart rehydration observes exactly the same spend.
Live charging rejects missing or all-zero usage and fails on an observed overrun.
Restart accounting sums decoded durable components; zero rows add no spend, and
production omits wholly unreported rows (which can include calls that never
started). This sum does not establish whether every completed call reported
usage. That admission obligation remains with the durable call owner.
-/

namespace PromptAssembly.AggregateBudget

structure Usage where
  inputTokens : Nat
  outputTokens : Nat
  reportedTotal : Nat
  deriving DecidableEq, Repr

/-- Charge the provider input/output components that are persisted durably.
`reportedTotal` remains an observed provider fact, not a second accounting
source with no corresponding durable column. -/
def Usage.chargedTotal (usage : Usage) : Nat :=
  usage.inputTokens + usage.outputTokens

/-- The usage facts retained on a terminal `InferenceCall`. Keeping this shape
separate makes the crash boundary explicit: rehydration sees these columns,
not the transient provider response. Production admits a row into this model
only when both durable components are present and non-negative; partial or
negative rows fail decoding. Wholly absent rows are omitted, and zero components
are allowed. Thus PersistedUsage models accounted rows, not call completion. -/
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

/-- InferenceCall accounting uses the exact physical request. Compaction and
ordinary inference share its allowance; logical retry labels, goals and sessions
must not widen the query. -/
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
never supplied by request input. Restart does not reread edited configuration.
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

/-- Neither call kind nor later retraction exempts a persisted charge. -/
theorem owned_call_counts (requestDocId : String) (kind : CallKind)
    (usage : PersistedUsage) (rows : List DurableCallUsage) :
    rehydrateUsed (requestUsage requestDocId (⟨requestDocId, kind, usage⟩ :: rows)) =
      usage.chargedTotal + rehydrateUsed (requestUsage requestDocId rows) := by
  simp [requestUsage, rehydrateUsed]

def Ledger.remaining (ledger : Ledger) : Nat :=
  ledger.limit - ledger.used

/-- Constrain a call's configured output ceiling by the request-wide budget
remaining after its assembled-input estimate. -/
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

/-- Post-call legality. Exact exhaustion may publish only an already-valid
terminal response; it cannot admit a tool turn, empty response, or failed
structured-output contract. Missing usage and observed overrun always fail. -/
def postChargeAction (result : ChargeResult) (terminalValid : Bool) : PostChargeAction :=
  match result with
  | .missing | .overrun _ => .fail
  | .within _ => if terminalValid then .succeed else .continue
  | .exhausted _ => if terminalValid then .succeed else .fail

/-- A missing or all-zero provider usage report is not enforceable. Otherwise
the post-call ledger distinguishes remaining capacity, exact exhaustion, and
an observed overrun. -/
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

/-- Once a charged provider result is durably appended, crash rehydration
recovers the prior spend plus exactly that charge. Production must await this
append before publishing the terminal stream item. -/
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

/-- Charging two calls is additive and order-independent. This is the key
retry property: a completed attempt that is later retracted still consumes the
same ledger as the replacement attempt. -/
theorem two_charges_are_additive
    (ledger : Ledger) (first second : Usage) :
    ((ledger.charge first).charge second).used =
      ledger.used + first.chargedTotal + second.chargedTotal := by
  rfl

end PromptAssembly.AggregateBudget
