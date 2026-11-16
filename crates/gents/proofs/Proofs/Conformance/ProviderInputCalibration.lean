import Proofs.Session.PromptAssembly.ContextCalibration
import Proofs.Conformance.Family

namespace Conformance.ProviderInputCalibration

open Lean PromptAssembly.ContextCalibration

structure ScalingSample where
  observed : Nat
  estimated : Nat
  deriving ToJson

structure ScalingInput where
  raw : Nat
  prior : Nat
  observations : List ScalingSample
  deriving ToJson

structure ScalingOutput where
  calibrated : Nat
  initialRatio : Nat
  ratio : Nat
  readings : List (Option Nat)
  deriving ToJson

@[conformance_family]
def scaling : Family ScalingInput ScalingOutput where
  inputs :=
    [ { raw := 561000, prior := 18000, observations := [] }
    , { raw := 3, prior := 15000, observations := [] }
    , { raw := 1000, prior := 5000, observations := [] }
    , { raw := 1000, prior := 1
      , observations := [⟨500, 1000⟩, ⟨1000000, 1⟩, ⟨0, 1000⟩, ⟨1000, 0⟩] } ]
  model := fun i =>
    let pairs := i.observations.map fun s => (s.observed, s.estimated)
    let final := pairs.foldl (fun (s : State) (p : Nat × Nat) => s.observe p.1 p.2) (seed i.prior [])
    { calibrated := calibrated i.raw final.ratio
      initialRatio := (seed i.prior []).ratio
      ratio := final.ratio
      readings := observeReadings (seed i.prior []) pairs }

end Conformance.ProviderInputCalibration
