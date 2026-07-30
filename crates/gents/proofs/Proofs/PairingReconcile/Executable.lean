import Proofs.PairingReconcile.Transition

namespace PairingReconcile

def domainName : String := "PairingReconcile"

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

inductive TransitionKind where
  | operatorWrite
  | operatorDelete
  | readFailure
  | dial
  | peerDisconnected
  | reconcileInstall
  | reconcileTeardown
  | reconcileInstallReplicator
  | reconcileTeardownReplicator
  | crash
  deriving DecidableEq, Repr

namespace TransitionKind

def fromString? : String → Option TransitionKind
  | "operatorWrite" => some .operatorWrite
  | "operatorDelete" => some .operatorDelete
  | "readFailure" => some .readFailure
  | "dial" => some .dial
  | "peerDisconnected" => some .peerDisconnected
  | "reconcileInstall" => some .reconcileInstall
  | "reconcileTeardown" => some .reconcileTeardown
  | "reconcileInstallReplicator" => some .reconcileInstallReplicator
  | "reconcileTeardownReplicator" => some .reconcileTeardownReplicator
  | "crash" => some .crash
  | _ => none

def toString : TransitionKind → String
  | .operatorWrite => "operatorWrite"
  | .operatorDelete => "operatorDelete"
  | .readFailure => "readFailure"
  | .dial => "dial"
  | .peerDisconnected => "peerDisconnected"
  | .reconcileInstall => "reconcileInstall"
  | .reconcileTeardown => "reconcileTeardown"
  | .reconcileInstallReplicator => "reconcileInstallReplicator"
  | .reconcileTeardownReplicator => "reconcileTeardownReplicator"
  | .crash => "crash"

theorem fromString_toString (k : TransitionKind) :
    fromString? k.toString = some k := by
  cases k <;> rfl

end TransitionKind

def step? : PairingPhase → TransitionKind → Option PairingPhase
  | .idle, .operatorWrite => some .diverged
  | .converged, .operatorWrite => some .diverged
  | .crashed, .operatorWrite => some .diverged
  | .idle, .operatorDelete => some .diverged
  | .converged, .operatorDelete => some .diverged
  | .crashed, .operatorDelete => some .diverged
  | phase, .readFailure => some phase
  | .diverged, .dial => some .converged
  | .converged, .peerDisconnected => some .diverged
  | .diverged, .peerDisconnected => some .diverged
  | .diverged, .reconcileInstall => some .converged
  | .diverged, .reconcileTeardown => some .converged
  | .diverged, .reconcileInstallReplicator => some .converged
  | .diverged, .reconcileTeardownReplicator => some .converged
  | _, .crash => some .crashed
  | _, _ => none

end PairingReconcile
