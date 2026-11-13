import Proofs.Runtime.Recovery.Sweeps.Registry
import Proofs.Conformance.Family

namespace Conformance.PeriodicRecovery

open Lean Recovery

structure SweepInput where
  sweepId : String
  deriving ToJson

structure SweepOutput where
  position : Option Nat
  rustFunction : Option String
  runsAfterSessionMessage : Bool
  deriving ToJson

@[conformance_family]
def registry : Family SweepInput SweepOutput where
  inputs := registeredRecoverySweepIds.map fun sweepId => { sweepId }
  model := fun i =>
    { position := periodicRecoveryTickPosition i.sweepId
      rustFunction := periodicRecoveryTickFunction i.sweepId
      runsAfterSessionMessage := runsAfterSessionMessageSweep i.sweepId }

end Conformance.PeriodicRecovery
