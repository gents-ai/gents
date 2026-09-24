import Proofs.Compaction.ReductionEngine

namespace Conformance.ContractCases.CompactionProjectionJoin
open Compaction.ReductionEngine

inductive FixtureRequest where
  | initial (source : List Nat)
  | rebuilt (checkpoint : Nat) (suffix : List Nat)
  deriving DecidableEq, Repr

inductive FixtureError where
  | initialProjection
  | initialEstimate (source : List Nat)
  | rebuild (checkpoint : Nat) (suffix : List Nat)
  | rebuiltEstimate (checkpoint : Nat) (suffix : List Nat)
  deriving DecidableEq, Repr

structure Case where
  name : String
  source : List Nat
  contextWindow : Nat := 100
  thresholdBasisPoints : Nat := 5000
  configuredMaxOutputTokens : Nat := 20
  canFit : Bool := true
  prefixLength : Nat := 1
  checkpoint : Nat := 90
  initialProjectionSucceeds : Bool := true
  initialEstimateTokens : Option Nat := some 51
  rebuildSucceeds : Bool := true
  rebuiltEstimateTokens : Option Nat := some 40

def project (w : Case) (source : List Nat) : Except FixtureError FixtureRequest :=
  if w.initialProjectionSucceeds then .ok (.initial source) else .error .initialProjection

def rebuild (w : Case) (checkpoint : Nat) (suffix : List Nat) : Except FixtureError FixtureRequest :=
  if w.rebuildSucceeds then .ok (.rebuilt checkpoint suffix) else .error (.rebuild checkpoint suffix)

def estimate (w : Case) : FixtureRequest → Except FixtureError Nat
  | .initial source => match w.initialEstimateTokens with
      | some tokens => .ok tokens | none => .error (.initialEstimate source)
  | .rebuilt checkpoint suffix => match w.rebuiltEstimateTokens with
      | some tokens => .ok tokens | none => .error (.rebuiltEstimate checkpoint suffix)

def result (w : Case) : Except FixtureError (RebuiltDispatch FixtureError FixtureRequest) :=
  reduceRebuildAndAuthorize (project w) (rebuild w) (estimate w) w.source w.contextWindow
    w.thresholdBasisPoints w.configuredMaxOutputTokens w.canFit w.prefixLength w.checkpoint

def resultName : Except FixtureError (RebuiltDispatch FixtureError FixtureRequest) → String
  | .error .initialProjection => "initial_projection_failed"
  | .error (.initialEstimate _) => "initial_estimate_failed"
  | .error (.rebuild _ _) => "rebuild_failed"
  | .error (.rebuiltEstimate _ _) => "rebuilt_estimate_failed"
  | .ok (.notReduced (.notNeeded _)) => "not_needed"
  | .ok (.notReduced .cannotFit) => "cannot_fit"
  | .ok (.notReduced (.reduced _ _ _)) => "unexpected_not_reduced"
  | .ok (.projectionFailed (.rebuild _ _)) => "rebuild_failed"
  | .ok (.projectionFailed (.rebuiltEstimate _ _)) => "rebuilt_estimate_failed"
  | .ok (.projectionFailed _) => "unexpected_projection_failure"
  | .ok (.stillOverThreshold _) => "rebuilt_still_over"
  | .ok (.noOutputCapacity _) => "zero_output_capacity"
  | .ok (.dispatch _ _) => "dispatch"

def rebuiltInput (w : Case) : Option (Nat × List Nat) :=
  match result w with
  | .ok (.projectionFailed (.rebuild checkpoint suffix))
  | .ok (.projectionFailed (.rebuiltEstimate checkpoint suffix)) => some (checkpoint, suffix)
  | .ok (.stillOverThreshold projected) | .ok (.noOutputCapacity projected)
  | .ok (.dispatch projected _) => some (projected.checkpoint, projected.retainedSuffix)
  | _ => none

def outputTokens (w : Case) : Option Nat :=
  match result w with | .ok (.dispatch _ tokens) => some tokens | _ => none

def cases : List Case :=
  [ { name := "initial_projection_failure", source := [1,2,3], initialProjectionSucceeds := false }
  , { name := "initial_estimate_failure", source := [1,2,3], initialEstimateTokens := none }
  , { name := "threshold_equality", source := [1,2,3], initialEstimateTokens := some 50 }
  , { name := "threshold_plus_one", source := [1,2,3] }
  , { name := "rebuild_failure", source := [1,2,3], rebuildSucceeds := false }
  , { name := "rebuilt_estimate_failure", source := [1,2,3], rebuiltEstimateTokens := none }
  , { name := "rebuilt_still_over", source := [1,2,3], rebuiltEstimateTokens := some 51 }
  , { name := "capacity_precedes_threshold", source := [1,2,3],
      rebuiltEstimateTokens := some 101, configuredMaxOutputTokens := 0 }
  , { name := "zero_output_capacity", source := [1,2,3], configuredMaxOutputTokens := 0 }
  , { name := "full_context_zero_output_capacity", source := [1,2,3], thresholdBasisPoints := 10000,
      initialEstimateTokens := some 101, rebuiltEstimateTokens := some 100 }
  , { name := "rebuilt_threshold_equality", source := [1,2,3], rebuiltEstimateTokens := some 50 }
  , { name := "dynamic_output_clamp", source := [1,2,3], configuredMaxOutputTokens := 80 }
  , { name := "invalid_zero_prefix", source := [1,2,3], prefixLength := 0 }
  , { name := "invalid_overlong_prefix", source := [1,2,3], prefixLength := 4 }
  , { name := "cannot_fit", source := [1,2,3], canFit := false } ]

theorem expected_results : cases.map (fun w => resultName (result w)) =
    ["initial_projection_failed", "initial_estimate_failed", "not_needed", "dispatch",
     "rebuild_failed", "rebuilt_estimate_failed", "rebuilt_still_over", "zero_output_capacity", "zero_output_capacity",
     "zero_output_capacity", "dispatch", "dispatch", "cannot_fit", "cannot_fit", "cannot_fit"] := by
  native_decide

end Conformance.ContractCases.CompactionProjectionJoin
