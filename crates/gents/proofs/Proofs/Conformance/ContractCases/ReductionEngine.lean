import Proofs.Conformance.ContractCases.Types
import Proofs.Compaction.ReductionEngine

namespace Conformance.ContractCases

open Compaction.ReductionEngine

structure ReductionEngineCase where
  name : String
  source : List Nat
  inputTokens : Nat
  effectiveInputBudget : Nat
  canFit : Bool
  prefixLength : Nat
  checkpoint : Nat
  thresholdDecision : String
  decision : String
  outcome : String
  notNeededMessages : List Nat
  compactedPrefix : List Nat
  retainedSuffix : List Nat
  outcomeCheckpoint : Option Nat
  exact : Bool
  deriving Repr

private def thresholdDecisionName : ThresholdDecision → String
  | .notNeeded => "not_needed"
  | .reduceEligible => "reduce_eligible"

private def decisionName : Decision → String
  | .notNeeded => "not_needed"
  | .cannotFit => "cannot_fit"
  | .reduce _ _ => "reduce"

private def outcomeName : Outcome → String
  | .notNeeded _ => "not_needed"
  | .cannotFit => "cannot_fit"
  | .reduced _ _ _ => "reduced"

private def notNeededMessages : Outcome → List Nat
  | .notNeeded messages => messages
  | _ => []

private def compactedPrefix : Outcome → List Nat
  | .reduced messages _ _ => messages
  | _ => []

private def retainedSuffix : Outcome → List Nat
  | .reduced _ messages _ => messages
  | _ => []

private def outcomeCheckpoint : Outcome → Option Nat
  | .reduced _ _ checkpoint => some checkpoint
  | _ => none

private structure ReductionWitness where
  name : String
  source : List Nat
  inputTokens : Nat
  effectiveInputBudget : Nat
  canFit : Bool
  prefixLength : Nat
  checkpoint : Nat

private def reductionWitnesses : List ReductionWitness :=
  [ { name := "below-threshold-is-not-needed", source := [10, 20, 30]
    , inputTokens := 4, effectiveInputBudget := 5, canFit := true
    , prefixLength := 1, checkpoint := 90 }
  , { name := "threshold-equality-is-not-needed", source := [10, 20, 30]
    , inputTokens := 5, effectiveInputBudget := 5, canFit := true
    , prefixLength := 1, checkpoint := 91 }
  , { name := "one-over-reduces-exact-prefix", source := [10, 20, 30, 40]
    , inputTokens := 6, effectiveInputBudget := 5, canFit := true
    , prefixLength := 2, checkpoint := 92 }
  , { name := "eligible-but-cannot-fit", source := [10, 20, 30, 40]
    , inputTokens := 6, effectiveInputBudget := 5, canFit := false
    , prefixLength := 2, checkpoint := 93 }
  , { name := "zero-prefix-cannot-fit", source := [10, 20, 30, 40]
    , inputTokens := 6, effectiveInputBudget := 5, canFit := true
    , prefixLength := 0, checkpoint := 94 }
  , { name := "overlong-prefix-cannot-fit", source := [10, 20, 30, 40]
    , inputTokens := 6, effectiveInputBudget := 5, canFit := true
    , prefixLength := 5, checkpoint := 95 }
  , { name := "full-prefix-is-exact", source := [10, 20, 30, 40]
    , inputTokens := 6, effectiveInputBudget := 5, canFit := true
    , prefixLength := 4, checkpoint := 96 }
  , { name := "empty-source-cannot-fit", source := []
    , inputTokens := 1, effectiveInputBudget := 0, canFit := true
    , prefixLength := 1, checkpoint := 97 }
  ]

private def reductionCase (witness : ReductionWitness) : ReductionEngineCase :=
  let threshold := decideThreshold witness.inputTokens witness.effectiveInputBudget
  let decision := reductionDecision witness.inputTokens witness.effectiveInputBudget
    witness.canFit witness.prefixLength witness.checkpoint
  let result := applyDecision witness.source decision
  { name := witness.name
  , source := witness.source
  , inputTokens := witness.inputTokens
  , effectiveInputBudget := witness.effectiveInputBudget
  , canFit := witness.canFit
  , prefixLength := witness.prefixLength
  , checkpoint := witness.checkpoint
  , thresholdDecision := thresholdDecisionName threshold
  , decision := decisionName decision
  , outcome := outcomeName result
  , notNeededMessages := notNeededMessages result
  , compactedPrefix := compactedPrefix result
  , retainedSuffix := retainedSuffix result
  , outcomeCheckpoint := outcomeCheckpoint result
  , exact := decide (ExactOutcome witness.source result)
  }

def reductionEngineCases : List ReductionEngineCase :=
  reductionWitnesses.map reductionCase

end Conformance.ContractCases
