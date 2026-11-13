import Proofs.Session.Compaction.ReductionEngine
import Proofs.Conformance.Family

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

def budget (w : Case) : Nat :=
  PromptAssembly.Budget.effectiveInputBudget
    (PromptAssembly.Budget.configuredThresholdBudget w.contextWindow w.thresholdBasisPoints)
    w.contextWindow

/-- Durable request-local reductions after the case, from an empty request. -/
def committedReductions (w : Case) : Nat :=
  match projectRequest (project w) (estimate w) w.source, result w with
  | .ok initial, .ok dispatch =>
      (commitRequestLocalAfterRebuild { reductions := [] }
        (reduce w.source initial.inputTokens (budget w) w.canFit w.prefixLength w.checkpoint)
        dispatch).reductions.length
  | _, _ => 0

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

theorem expected_committed_reductions : cases.map committedReductions =
    [0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0] := by
  native_decide

structure JoinInput where
  name : String
  source : List Nat
  contextWindow : Nat
  thresholdBasisPoints : Nat
  configuredMaxOutputTokens : Nat
  canFit : Bool
  prefixLength : Nat
  checkpoint : Nat
  initialProjectionSucceeds : Bool
  initialEstimateTokens : Option Nat
  rebuildSucceeds : Bool
  rebuiltEstimateTokens : Option Nat
  deriving Lean.ToJson

def JoinInput.ofCase (w : Case) : JoinInput :=
  { name := w.name, source := w.source, contextWindow := w.contextWindow
    thresholdBasisPoints := w.thresholdBasisPoints
    configuredMaxOutputTokens := w.configuredMaxOutputTokens, canFit := w.canFit
    prefixLength := w.prefixLength, checkpoint := w.checkpoint
    initialProjectionSucceeds := w.initialProjectionSucceeds
    initialEstimateTokens := w.initialEstimateTokens, rebuildSucceeds := w.rebuildSucceeds
    rebuiltEstimateTokens := w.rebuiltEstimateTokens }

def JoinInput.toCase (i : JoinInput) : Case :=
  { name := i.name, source := i.source, contextWindow := i.contextWindow
    thresholdBasisPoints := i.thresholdBasisPoints
    configuredMaxOutputTokens := i.configuredMaxOutputTokens, canFit := i.canFit
    prefixLength := i.prefixLength, checkpoint := i.checkpoint
    initialProjectionSucceeds := i.initialProjectionSucceeds
    initialEstimateTokens := i.initialEstimateTokens, rebuildSucceeds := i.rebuildSucceeds
    rebuiltEstimateTokens := i.rebuiltEstimateTokens }

structure RebuiltInput where
  checkpoint : Nat
  retainedSuffix : List Nat
  deriving Lean.ToJson

structure JoinOutput where
  initialProjectionInput : List Nat
  rebuiltProjectionInput : Option RebuiltInput
  result : String
  outputTokens : Option Nat
  committedReductions : Nat
  deriving Lean.ToJson

/-- model-production-gap: no native adapter drives the projection join with
injected projection, rebuild and estimate observations, so no production path
replays these cases; `stagedTurnReduction` replays the three outcomes the
per-turn staged reduction reaches. -/
@[conformance_family]
def projectionJoin : Family JoinInput JoinOutput where
  inputs := cases.map JoinInput.ofCase
  model := fun i =>
    let w := i.toCase
    { initialProjectionInput := w.source
      rebuiltProjectionInput := (rebuiltInput w).map fun (checkpoint, retainedSuffix) =>
        { checkpoint, retainedSuffix }
      result := resultName (result w)
      outputTokens := outputTokens w
      committedReductions := committedReductions w }

structure StagedTurnOutput where
  result : String
  committedReductions : Nat
  deriving Lean.ToJson

/-- The served per-turn staged reduction builds a request whose rebuilt
projection still exceeds the threshold, one with no output capacity, and one
that dispatches; it observes the loop's outcome and the durable reduction rows,
not the injected projection and estimate observations. -/
@[conformance_family]
def stagedTurnReduction : Family JoinInput StagedTurnOutput where
  inputs := (cases.filter fun w =>
    w.name ∈ ["threshold_plus_one", "rebuilt_still_over", "zero_output_capacity"]).map
      JoinInput.ofCase
  model := fun i =>
    let w := i.toCase
    { result := resultName (result w), committedReductions := committedReductions w }

end Conformance.ContractCases.CompactionProjectionJoin
