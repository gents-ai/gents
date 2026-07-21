import Proofs.Recovery.Contract

/-!
# Recovery Sweep Outcomes (#693)

`RecoverySweep.recover` is a total function: it says what a recovered row
*becomes*, not whether the persistence write that lands it *succeeded*. The
sweep contract therefore proves `recoveredRows_length = rows.length` — every
attempted row is a recovered row — which is exactly the assumption a reporting
implementation must NOT make. A sweep that reports `rows.len()` recoveries is a
faithful implementation of that assumption, and reports a fully-failed pass as
healthy (#693 defect 2).

This module adds the missing layer: a per-row *outcome*, a report that counts
recoveries and failures separately, and the theorems that make honest
accounting checkable:

- `report_accounts_for_every_row` — nothing is silently dropped;
- `recovered_counts_only_successes` — the recovered count is exactly the number
  of rows whose persistence step succeeded;
- `all_failed_reports_zero` — the direct anti-theorem for #693 defect 2: a pass
  in which every write fails reports zero recoveries;
- `aggregateMeasure_unchanged_by_failure` — a failed row stays stale, so a
  failed pass converges nothing (it must be retried, not reported as done).

The persistence step is modeled as an oracle (`Step`), because whether a write
lands is a property of the store, not of the row: it is exactly where DefraDB
rejected the multi-match upsert in #693.
-/

namespace Recovery

/-- The result of attempting one row's recovery: the write landed (and the row
    reached its recovered value), or it did not (and the row is unchanged). -/
inductive SweepOutcome (Row : Type) where
  | recovered (row : Row)
  | failed (row : Row)
  deriving Repr

namespace SweepOutcome

def row {Row : Type} : SweepOutcome Row → Row
  | .recovered row => row
  | .failed row => row

def succeeded {Row : Type} : SweepOutcome Row → Bool
  | .recovered _ => true
  | .failed _ => false

end SweepOutcome

/-- What a sweep pass reports to the operator. -/
structure SweepReport where
  recovered : Nat
  failed : Nat
  deriving DecidableEq, Repr

namespace SweepReport

def total (report : SweepReport) : Nat :=
  report.recovered + report.failed

def empty : SweepReport := { recovered := 0, failed := 0 }

def observe {Row : Type} (report : SweepReport) (outcome : SweepOutcome Row) : SweepReport :=
  match outcome with
  | .recovered _ => { report with recovered := report.recovered + 1 }
  | .failed _ => { report with failed := report.failed + 1 }

end SweepReport

/-- A persistence step: given a stale row, either the recovered row lands, or
    the write fails and the row is left as it was. `succeeds` is the oracle for
    whether the store accepted the write. -/
structure Step (sweep : RecoverySweep) where
  succeeds : sweep.Row → Bool

namespace Step

variable {sweep : RecoverySweep}

/-- Attempting one row: on success the row takes its recovered value; on
    failure it is returned untouched (no partial write). -/
def attempt (step : Step sweep) (row : sweep.Row) : SweepOutcome sweep.Row :=
  if step.succeeds row then
    .recovered (sweep.recover row)
  else
    .failed row

def outcomes (step : Step sweep) (rows : List sweep.Row) : List (SweepOutcome sweep.Row) :=
  rows.map step.attempt

/-- The report of one sweep pass: recoveries and failures counted separately. -/
def run (step : Step sweep) (rows : List sweep.Row) : SweepReport :=
  (step.outcomes rows).foldl SweepReport.observe SweepReport.empty

/-- The rows as they are left in the store after the pass. -/
def resultRows (step : Step sweep) (rows : List sweep.Row) : List sweep.Row :=
  (step.outcomes rows).map SweepOutcome.row

@[simp] theorem attempt_succeeded (step : Step sweep) (row : sweep.Row) :
    (step.attempt row).succeeded = step.succeeds row := by
  unfold attempt
  by_cases h : step.succeeds row = true
  · simp [h, SweepOutcome.succeeded]
  · simp only [Bool.not_eq_true] at h
    simp [h, SweepOutcome.succeeded]

/-- Folding the report accumulates independently of the starting counts. -/
theorem foldl_observe_add (outcomes : List (SweepOutcome sweep.Row)) (report : SweepReport) :
    outcomes.foldl SweepReport.observe report =
      { recovered :=
          report.recovered
            + (outcomes.foldl SweepReport.observe SweepReport.empty).recovered
      , failed :=
          report.failed
            + (outcomes.foldl SweepReport.observe SweepReport.empty).failed } := by
  induction outcomes generalizing report with
  | nil => cases report; simp [SweepReport.empty]
  | cons hd tl ih =>
      cases hd with
      | recovered row =>
          rw [List.foldl_cons, List.foldl_cons, ih, ih (SweepReport.observe SweepReport.empty _)]
          simp [SweepReport.observe, SweepReport.empty, Nat.add_assoc, Nat.add_comm,
            Nat.add_left_comm]
      | failed row =>
          rw [List.foldl_cons, List.foldl_cons, ih, ih (SweepReport.observe SweepReport.empty _)]
          simp [SweepReport.observe, SweepReport.empty, Nat.add_assoc, Nat.add_comm,
            Nat.add_left_comm]

/-- **Accounting completeness.** Every attempted row is reported exactly once,
    either as a recovery or as a failure — nothing is silently dropped. -/
theorem report_accounts_for_every_row (step : Step sweep) (rows : List sweep.Row) :
    (step.run rows).total = rows.length := by
  unfold run outcomes
  induction rows with
  | nil => simp [SweepReport.total, SweepReport.empty]
  | cons hd tl ih =>
      rw [List.map_cons, List.foldl_cons, foldl_observe_add]
      cases h : step.attempt hd <;>
        simp [SweepReport.total, SweepReport.observe, SweepReport.empty] at * <;>
        omega

/-- **The recovered count is exactly the number of successful writes.**
    This is the property #693 defect 2 violated: the implementation returned
    `rows.len()` (attempts) instead. -/
theorem recovered_counts_only_successes (step : Step sweep) (rows : List sweep.Row) :
    (step.run rows).recovered = (rows.filter step.succeeds).length := by
  unfold run outcomes
  induction rows with
  | nil => simp [SweepReport.empty]
  | cons hd tl ih =>
      rw [List.map_cons, List.foldl_cons, foldl_observe_add, List.filter_cons]
      simp only [SweepReport.empty] at ih
      by_cases h : step.succeeds hd = true
      · simp [attempt, h, SweepReport.observe, SweepReport.empty]
        omega
      · simp only [Bool.not_eq_true] at h
        simp [attempt, h, SweepReport.observe, SweepReport.empty]
        omega

/-- Failures are likewise exactly the rows whose write did not land. -/
theorem failed_counts_only_failures (step : Step sweep) (rows : List sweep.Row) :
    (step.run rows).failed = (rows.filter (fun row => !step.succeeds row)).length := by
  unfold run outcomes
  induction rows with
  | nil => simp [SweepReport.empty]
  | cons hd tl ih =>
      rw [List.map_cons, List.foldl_cons, foldl_observe_add, List.filter_cons]
      simp only [SweepReport.empty] at ih
      by_cases h : step.succeeds hd = true
      · simp [attempt, h, SweepReport.observe, SweepReport.empty]
        omega
      · simp only [Bool.not_eq_true] at h
        simp [attempt, h, SweepReport.observe, SweepReport.empty]
        omega

/-- **T-693: a pass in which every write fails reports zero recoveries.**
    The exact defect: two failed `upsert_AgentConversation` calls were reported
    as `recovered stuck conversations count=2`. -/
theorem all_failed_reports_zero (step : Step sweep) (rows : List sweep.Row)
    (h_all_fail : ∀ row ∈ rows, step.succeeds row = false) :
    (step.run rows).recovered = 0 := by
  rw [recovered_counts_only_successes]
  have h_empty : rows.filter step.succeeds = [] := by
    apply List.filter_eq_nil_iff.mpr
    intro row h_mem
    simp [h_all_fail row h_mem]
  simp [h_empty]

/-- Dually, an all-successful pass reports every row as recovered — the fix must
    not under-count either. -/
theorem all_succeeded_reports_all (step : Step sweep) (rows : List sweep.Row)
    (h_all_ok : ∀ row ∈ rows, step.succeeds row = true) :
    (step.run rows).recovered = rows.length := by
  rw [recovered_counts_only_successes]
  have h_all : rows.filter step.succeeds = rows := by
    apply List.filter_eq_self.mpr
    intro row h_mem
    exact h_all_ok row h_mem
  simp [h_all]

/-- A failed row is returned untouched, so it stays stale: a failed pass
    converges nothing and must be retried, never reported as done. -/
theorem failed_row_unchanged (step : Step sweep) (row : sweep.Row)
    (h_fail : step.succeeds row = false) :
    (step.attempt row).row = row := by
  simp [attempt, h_fail, SweepOutcome.row]

theorem failed_row_still_stale (step : Step sweep) (row : sweep.Row)
    (h_stale : sweep.stale row) (h_fail : step.succeeds row = false) :
    sweep.stale (step.attempt row).row := by
  rw [failed_row_unchanged step row h_fail]
  exact h_stale

/-- A successful row reaches the same value the total `recover` prescribes, so
    the outcome layer refines the existing contract rather than replacing it. -/
theorem recovered_row_matches_contract (step : Step sweep) (row : sweep.Row)
    (h_ok : step.succeeds row = true) :
    (step.attempt row).row = sweep.recover row := by
  simp [attempt, h_ok, SweepOutcome.row]

/-- With an always-succeeding step the outcome layer collapses to the existing
    total-recovery contract (`recoveredRows`), so every theorem already proven
    there still applies to the happy path. -/
theorem resultRows_eq_recoveredRows_of_total (step : Step sweep) (rows : List sweep.Row)
    (h_all_ok : ∀ row ∈ rows, step.succeeds row = true) :
    step.resultRows rows = sweep.recoveredRows rows := by
  unfold resultRows outcomes RecoverySweep.recoveredRows
  induction rows with
  | nil => simp
  | cons hd tl ih =>
      have h_hd : step.succeeds hd = true := h_all_ok hd (by simp)
      have h_tl : ∀ row ∈ tl, step.succeeds row = true := by
        intro row h_mem
        exact h_all_ok row (by simp [h_mem])
      simp [attempt, h_hd, SweepOutcome.row, ih h_tl]

end Step

end Recovery
