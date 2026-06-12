import Proofs.PairingReconcile.Transition

/-!
# Pairing Reconcile Executable Contract

Small executable vocabulary consumed by the Rust conformance bridge.
-/

namespace PairingReconcile

def domainName : String := "PairingReconcile"

/-- Coarse phase vocabulary for contract extraction. -/
inductive PairingPhase where
  | idle
  | diverged
  | converged
  | crashed
  deriving DecidableEq, Repr

namespace PairingPhase

def toContract : PairingPhase → String
  | .idle => "idle"
  | .diverged => "diverged"
  | .converged => "converged"
  | .crashed => "crashed"

def fromContract? : String → Option PairingPhase
  | "idle" => some .idle
  | "diverged" => some .diverged
  | "converged" => some .converged
  | "crashed" => some .crashed
  | _ => none

theorem fromContract_toContract (phase : PairingPhase) :
    fromContract? phase.toContract = some phase := by
  cases phase <;> rfl

end PairingPhase

/-- Stringly-typed transition kinds emitted by the supervisor. -/
inductive TransitionKind where
  | operatorWrite
  | reconcileInstall
  | reconcileTeardown
  | reconcileInstallReplicator
  | reconcileTeardownReplicator
  | crash
  deriving DecidableEq, Repr

namespace TransitionKind

def fromString? : String → Option TransitionKind
  | "operatorWrite" => some .operatorWrite
  | "reconcileInstall" => some .reconcileInstall
  | "reconcileTeardown" => some .reconcileTeardown
  | "reconcileInstallReplicator" => some .reconcileInstallReplicator
  | "reconcileTeardownReplicator" => some .reconcileTeardownReplicator
  | "crash" => some .crash
  | _ => none

def toString : TransitionKind → String
  | .operatorWrite => "operatorWrite"
  | .reconcileInstall => "reconcileInstall"
  | .reconcileTeardown => "reconcileTeardown"
  | .reconcileInstallReplicator => "reconcileInstallReplicator"
  | .reconcileTeardownReplicator => "reconcileTeardownReplicator"
  | .crash => "crash"

theorem fromString_toString (k : TransitionKind) :
    fromString? k.toString = some k := by
  cases k <;> rfl

end TransitionKind

/-- Executable coarse transition relation for conformance extraction. -/
def step? : PairingPhase → TransitionKind → Option PairingPhase
  | .idle, .operatorWrite => some .diverged
  | .converged, .operatorWrite => some .diverged
  | .crashed, .operatorWrite => some .diverged
  | .diverged, .reconcileInstall => some .converged
  | .diverged, .reconcileTeardown => some .converged
  | .diverged, .reconcileInstallReplicator => some .converged
  | .diverged, .reconcileTeardownReplicator => some .converged
  | _, .crash => some .crashed
  | _, _ => none

end PairingReconcile
