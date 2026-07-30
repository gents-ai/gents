import Proofs.Recovery.Contract

namespace Recovery

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

structure Step (sweep : RecoverySweep) where
  succeeds : sweep.Row → Bool

namespace Step

variable {sweep : RecoverySweep}

def attempt (step : Step sweep) (row : sweep.Row) : SweepOutcome sweep.Row :=
  if step.succeeds row then
    .recovered (sweep.recover row)
  else
    .failed row

def outcomes (step : Step sweep) (rows : List sweep.Row) : List (SweepOutcome sweep.Row) :=
  rows.map step.attempt

def run (step : Step sweep) (rows : List sweep.Row) : SweepReport :=
  (step.outcomes rows).foldl SweepReport.observe SweepReport.empty

def resultRows (step : Step sweep) (rows : List sweep.Row) : List sweep.Row :=
  (step.outcomes rows).map SweepOutcome.row

@[simp] theorem attempt_succeeded (step : Step sweep) (row : sweep.Row) :
    (step.attempt row).succeeded = step.succeeds row := by
  unfold attempt
  by_cases h : step.succeeds row = true
  · simp [h, SweepOutcome.succeeded]
  · simp only [Bool.not_eq_true] at h
    simp [h, SweepOutcome.succeeded]

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

theorem all_failed_reports_zero (step : Step sweep) (rows : List sweep.Row)
    (h_all_fail : ∀ row ∈ rows, step.succeeds row = false) :
    (step.run rows).recovered = 0 := by
  rw [recovered_counts_only_successes]
  have h_empty : rows.filter step.succeeds = [] := by
    apply List.filter_eq_nil_iff.mpr
    intro row h_mem
    simp [h_all_fail row h_mem]
  simp [h_empty]

theorem all_succeeded_reports_all (step : Step sweep) (rows : List sweep.Row)
    (h_all_ok : ∀ row ∈ rows, step.succeeds row = true) :
    (step.run rows).recovered = rows.length := by
  rw [recovered_counts_only_successes]
  have h_all : rows.filter step.succeeds = rows := by
    apply List.filter_eq_self.mpr
    intro row h_mem
    exact h_all_ok row h_mem
  simp [h_all]

theorem failed_row_unchanged (step : Step sweep) (row : sweep.Row)
    (h_fail : step.succeeds row = false) :
    (step.attempt row).row = row := by
  simp [attempt, h_fail, SweepOutcome.row]

theorem failed_row_still_stale (step : Step sweep) (row : sweep.Row)
    (h_stale : sweep.stale row) (h_fail : step.succeeds row = false) :
    sweep.stale (step.attempt row).row := by
  rw [failed_row_unchanged step row h_fail]
  exact h_stale

theorem recovered_row_matches_contract (step : Step sweep) (row : sweep.Row)
    (h_ok : step.succeeds row = true) :
    (step.attempt row).row = sweep.recover row := by
  simp [attempt, h_ok, SweepOutcome.row]

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
