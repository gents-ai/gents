import Proofs.CanonicalOutput.Execution.CompactionCases

namespace Conformance.ContractCases.CompactionCanonicalProjection

open CanonicalOutput
open CanonicalOutput.Execution.Compaction
open CanonicalOutput.Execution.Compaction.Examples
open CanonicalOutput.Execution.Examples
open Compaction.ReductionEngine

inductive Request where
  | initial (native : List ReconstructedMessage)
  | rebuilt (checkpoint : Nat) (native : List ReconstructedMessage)
  | fresh (source : List Nat)
  deriving DecidableEq, Repr

inductive Error where
  | reconstruction
  | projection
  | estimate
  deriving DecidableEq, Repr

def project (_fixed : Unit) (native : List ReconstructedMessage) : Except Error Request :=
  .ok (.initial native)

def rebuild (_fixed : Unit) (checkpoint : Nat) (native : List ReconstructedMessage) :
    Except Error Request := .ok (.rebuilt checkpoint native)

def selected : List DocId :=
  [providerMessage.header.id, resultMessage.header.id, boundary.header.id]

def canonicalResultName : Except Error (RebuiltDispatch Error Request) → String
  | .error .reconstruction => "reconstruction_error"
  | .error .projection => "projection_error"
  | .error .estimate => "estimate_error"
  | .ok (.dispatch _ _) => "dispatch"
  | .ok (.stillOverThreshold _) => "over_threshold"
  | .ok (.noOutputCapacity _) => "no_output_capacity"
  | .ok (.notReduced (.notNeeded _)) => "not_needed"
  | .ok (.notReduced .cannotFit) => "cannot_fit"
  | .ok (.notReduced (.reduced _ _ _)) => "unexpected_not_reduced"
  | .ok (.projectionFailed _) => "rebuilt_projection_error"

def rebuiltNative : Except Error (RebuiltDispatch Error Request) →
    Option (Nat × List ReconstructedMessage)
  | .ok (.dispatch projected _) | .ok (.stillOverThreshold projected)
  | .ok (.noOutputCapacity projected) => match projected.request with
      | .rebuilt checkpoint trace => some (checkpoint, trace)
      | _ => none
  | _ => none

structure FreshCase where
  name : String
  source : List Nat := [7, 8]
  projectSucceeds : Bool := true
  estimateTokens : Option Nat := some 50
  contextWindow : Nat := 100
  effectiveInputBudget : Nat := 50
  configuredMaxOutputTokens : Nat := 20
  deriving Repr

def freshProject (w : FreshCase) (source : List Nat) : Except Error Request :=
  if w.projectSucceeds then .ok (.fresh source) else .error .projection

def freshEstimate (w : FreshCase) : Request → Except Error Nat
  | .fresh _ => match w.estimateTokens with
      | some tokens => .ok tokens
      | none => .error .estimate
  | _ => .error .estimate

def freshResult (w : FreshCase) : Except Error (FreshAdmission Request) :=
  projectAndAuthorize (freshProject w) (freshEstimate w) w.source
    w.effectiveInputBudget w.contextWindow w.configuredMaxOutputTokens

def freshResultName : Except Error (FreshAdmission Request) → String
  | .error .projection => "projection_error"
  | .error .estimate => "estimate_error"
  | .error .reconstruction => "reconstruction_error"
  | .ok (.overThreshold _) => "over_threshold"
  | .ok (.noOutputCapacity _) => "no_output_capacity"
  | .ok (.dispatch _ _) => "dispatch"

def freshOutput : Except Error (FreshAdmission Request) → Option Nat
  | .ok (.dispatch _ output) => some output
  | _ => none

def freshCases : List FreshCase :=
  [ { name := "repaired_projection_failure", projectSucceeds := false }
  , { name := "repaired_estimate_failure", estimateTokens := none }
  , { name := "repaired_threshold_equality" }
  , { name := "repaired_over_threshold", estimateTokens := some 51 }
  , { name := "repaired_zero_output_capacity", configuredMaxOutputTokens := 0 }
  , { name := "repaired_dynamic_output_clamp", estimateTokens := some 90,
      effectiveInputBudget := 90, configuredMaxOutputTokens := 80 } ]

structure CanonicalCase where
  name : String
  world : CanonicalOutput.Execution.World
  source : List Nat
  contextWindow : Nat := 100
  thresholdBasisPoints : Nat := 5000
  configuredMaxOutputTokens : Nat := 20
  canFit : Bool := true
  prefixLength : Nat := 2
  checkpoint : Nat := 90
  initialEstimateTokens : Nat := 51
  rebuiltEstimateTokens : Nat := 40

def caseEstimate (w : CanonicalCase) : Request → Except Error Nat
  | .initial _ => .ok w.initialEstimateTokens
  | .rebuilt _ _ => .ok w.rebuiltEstimateTokens
  | .fresh _ => .error .estimate

def canonicalRun (w : CanonicalCase) : Except Error (RebuiltDispatch Error Request) :=
  reduceCanonicalRebuildAndAuthorize w.world () project rebuild (caseEstimate w) .reconstruction
    w.source w.contextWindow w.thresholdBasisPoints w.configuredMaxOutputTokens w.canFit
    w.prefixLength w.checkpoint

def canonicalInputNative (w : CanonicalCase) : Option (List ReconstructedMessage) :=
  match canonicalProviderRequest w.world () project .reconstruction w.source with
  | .ok (.initial native) => some native
  | _ => none

def canonicalOutputTokens : Except Error (RebuiltDispatch Error Request) → Option Nat
  | .ok (.dispatch _ output) => some output
  | _ => none

def canonicalCases : List CanonicalCase :=
  [ { name := "canonical_composed_success", world := published, source := selected }
  , { name := "canonical_missing_later_selected_id", world := published
      source := [providerMessage.header.id, 999999] }
  , { name := "canonical_loading_dependency"
      world := { published with segments := [providerTurn, authored] }
      source := [providerMessage.header.id, resultMessage.header.id] } ]

def freshProjectedSource : Except Error (FreshAdmission Request) → Option (List Nat)
  | .ok (.overThreshold projected) | .ok (.noOutputCapacity projected)
  | .ok (.dispatch projected _) => some projected.source
  | _ => none

theorem canonical_expectations :
    canonicalCases.map (fun w => (canonicalResultName (canonicalRun w),
      (canonicalInputNative w).isSome, (rebuiltNative (canonicalRun w)).isSome,
      canonicalOutputTokens (canonicalRun w))) =
    [("dispatch", true, true, some 20),
     ("reconstruction_error", false, false, none),
     ("reconstruction_error", false, false, none)] := by native_decide

theorem fresh_expectations : freshCases.map (fun w =>
    (freshResultName (freshResult w), freshOutput (freshResult w))) =
    [("projection_error", none), ("estimate_error", none), ("dispatch", some 20),
     ("over_threshold", none), ("no_output_capacity", none), ("dispatch", some 10)] := by
  native_decide

end Conformance.ContractCases.CompactionCanonicalProjection
