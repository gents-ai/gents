import Proofs.Session.PromptAssembly.Budget
import Proofs.Conformance.Family

namespace Conformance.PromptAssemblyBudget

open Lean

structure RetentionInput where
  name : String
  configuredKeepRecent : Nat
  effectiveInputBudget : Nat
  fixedInput : Nat
  summaryMaxOutput : Nat
  /-- A bound, not an expectation: the retained tail never exceeds the input
  budget left after the fixed layers. -/
  availableInput : Nat
  deriving ToJson

structure RetentionOutput where
  effectiveSummaryOutput : Nat
  recoveryBand : Nat
  retentionTarget : Nat
  rollingSummaryInputBudget : Nat
  deriving ToJson

private def retentionInput
    (name : String) (configuredKeepRecent effectiveInputBudget fixedInput summaryMaxOutput : Nat) :
    RetentionInput :=
  { name, configuredKeepRecent, effectiveInputBudget, fixedInput, summaryMaxOutput
    availableInput := effectiveInputBudget - fixedInput }

@[conformance_family]
def retention : Family RetentionInput RetentionOutput where
  inputs :=
    [ retentionInput "zero-capacity" 0 0 0 0
    , retentionInput "one-token-capacity" 1 1 0 1
    , retentionInput "configured-cap-wins" 2 100 0 2
    , retentionInput "fifth-boundary-minus-one" 100 4 0 1
    , retentionInput "fifth-boundary-exact" 100 5 0 5
    , retentionInput "fifth-boundary-one-over" 100 6 0 6
    , retentionInput "fixed-input-leaves-one" 100 100 99 25
    , retentionInput "fixed-input-consumes-capacity" 100 100 100 100
    , retentionInput "fixed-input-exceeds-capacity" 100 100 101 101
    , retentionInput "large-uncapped-json-safe" 200000 200000 1 32768
    , retentionInput "large-configured-cap" 20000 200000 58000 200000
    , retentionInput "small-context-large-summary-ceiling" 20 2000 0 32768
    , retentionInput "summary-ceiling-smaller-than-fifth" 20 6000 0 512
    , retentionInput "recovery-band-750k-budget" 1000000 750000 50000 32000
    , retentionInput "summary-reserve-exhausts-band" 1000000 1000 500 300
    ]
  model := fun i =>
    let summaryOutput :=
      PromptAssembly.Budget.summaryOutputCeiling i.summaryMaxOutput i.effectiveInputBudget
    { effectiveSummaryOutput := summaryOutput
      recoveryBand := PromptAssembly.Budget.recoveryBandBudget i.effectiveInputBudget
      retentionTarget := PromptAssembly.Budget.compactionRetentionTarget
        i.configuredKeepRecent i.effectiveInputBudget i.fixedInput summaryOutput
      rollingSummaryInputBudget :=
        PromptAssembly.Budget.rollingSummaryInputBudget i.effectiveInputBudget summaryOutput }

end Conformance.PromptAssemblyBudget
