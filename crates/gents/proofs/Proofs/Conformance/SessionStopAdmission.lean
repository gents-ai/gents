import Proofs.Automation.Session.Control
import Proofs.Conformance.Family

namespace Conformance.SessionStopAdmission

open Lean SessionQueue.Control

structure StopInput where
  pendingCount : Nat
  deriving ToJson

structure StopOutput where
  stopAccepted : Bool
  drainedPending : Bool
  stopped : Bool
  lateMessageRefused : Bool
  deriving ToJson

@[conformance_family]
def drains : Family StopInput StopOutput where
  inputs := [{ pendingCount := 1 }]
  model := fun i =>
    { stopAccepted := stopAcceptedAt i.pendingCount
      drainedPending := drainedPendingAt i.pendingCount
      stopped := stoppedAfterStop i.pendingCount
      lateMessageRefused := lateMessageRefusedAfterStop i.pendingCount }

end Conformance.SessionStopAdmission
