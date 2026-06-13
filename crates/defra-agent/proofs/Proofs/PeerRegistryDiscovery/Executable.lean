import Proofs.PeerRegistryDiscovery.Transition

/-!
# Peer Registry Discovery — Executable Contract

Small executable vocabulary consumed by the Rust conformance bridge (R5).
Mirrors `PairingReconcile/Executable.lean`: a `TransitionKind` vocabulary and a
coarse phase machine with `toContract`/`fromContract?` round-trips.
-/

namespace PeerRegistryDiscovery

def domainName : String := "PeerRegistryDiscovery"

/-- Coarse phase vocabulary for contract extraction. -/
inductive DiscoveryPhase where
  | unsettled
  | settled
  deriving DecidableEq, Repr

namespace DiscoveryPhase

def toContract : DiscoveryPhase → String
  | .unsettled => "unsettled"
  | .settled => "settled"

def fromContract? : String → Option DiscoveryPhase
  | "unsettled" => some .unsettled
  | "settled" => some .settled
  | _ => none

theorem fromContract_toContract (phase : DiscoveryPhase) :
    fromContract? phase.toContract = some phase := by
  cases phase <;> rfl

end DiscoveryPhase

/-- Stringly-typed transition kinds emitted by the discovery reconciler. -/
inductive TransitionKind where
  | derive
  | join
  | removeEntry
  | operatorWrite
  deriving DecidableEq, Repr

namespace TransitionKind

def fromString? : String → Option TransitionKind
  | "derive" => some .derive
  | "join" => some .join
  | "removeEntry" => some .removeEntry
  | "operatorWrite" => some .operatorWrite
  | _ => none

def toString : TransitionKind → String
  | .derive => "derive"
  | .join => "join"
  | .removeEntry => "removeEntry"
  | .operatorWrite => "operatorWrite"

theorem fromString_toString (k : TransitionKind) :
    fromString? k.toString = some k := by
  cases k <;> rfl

end TransitionKind

/-- Executable coarse transition relation for conformance extraction. A derive
step settles; a registry edit (join/removeEntry) unsettles the derived view;
an operator write leaves the derived view as-is. -/
def step? : DiscoveryPhase → TransitionKind → Option DiscoveryPhase
  | _, .derive => some .settled
  | _, .join => some .unsettled
  | _, .removeEntry => some .unsettled
  | phase, .operatorWrite => some phase

end PeerRegistryDiscovery
