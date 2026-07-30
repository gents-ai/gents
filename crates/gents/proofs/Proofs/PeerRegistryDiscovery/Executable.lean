import Proofs.PeerRegistryDiscovery.Transition

namespace PeerRegistryDiscovery

def domainName : String := "PeerRegistryDiscovery"

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

inductive TransitionKind where
  | derive
  | join
  | reciprocalJoin
  | removeEntry
  | operatorWrite
  deriving DecidableEq, Repr

namespace TransitionKind

def fromString? : String → Option TransitionKind
  | "derive" => some .derive
  | "join" => some .join
  | "reciprocalJoin" => some .reciprocalJoin
  | "removeEntry" => some .removeEntry
  | "operatorWrite" => some .operatorWrite
  | _ => none

def toString : TransitionKind → String
  | .derive => "derive"
  | .join => "join"
  | .reciprocalJoin => "reciprocalJoin"
  | .removeEntry => "removeEntry"
  | .operatorWrite => "operatorWrite"

theorem fromString_toString (k : TransitionKind) :
    fromString? k.toString = some k := by
  cases k <;> rfl

end TransitionKind

def step? : DiscoveryPhase → TransitionKind → Option DiscoveryPhase
  | _, .derive => some .settled
  | _, .join => some .unsettled
  | _, .reciprocalJoin => some .unsettled
  | _, .removeEntry => some .unsettled
  | phase, .operatorWrite => some phase

def decideAdmitsJoin (s : DiscoveryState) (tok : Token) (tofuBootstrap : Bool) : Bool :=
  decide (admitsJoin s tok tofuBootstrap)

theorem decideAdmitsJoin_agrees (s : DiscoveryState) (tok : Token) (tofuBootstrap : Bool) :
    decideAdmitsJoin s tok tofuBootstrap = true ↔ admitsJoin s tok tofuBootstrap := by
  unfold decideAdmitsJoin
  exact decide_eq_true_iff

end PeerRegistryDiscovery
