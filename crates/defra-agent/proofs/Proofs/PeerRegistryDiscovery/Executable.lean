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

/-! ## Executable join-admission decision (mirrors Rust `decide_join_admission`)

The Rust bridge decides whether a join is admissible with a single boolean. This
function is that boolean, and it now threads the single-use nonce check: a join
is admitted iff it is member-signed (or TOFU-bootstrapped) AND the token's nonce
has not already been consumed. `decideAdmitsJoin_agrees` fences it to the
`admitsJoin` Prop the `Transition.join` constructor requires, so the executable
decision and the relation can never diverge on replay. -/
def decideAdmitsJoin (s : DiscoveryState) (tok : Token) (tofuBootstrap : Bool) : Bool :=
  decide (admitsJoin s tok tofuBootstrap)

/-- The executable decision agrees exactly with the `admitsJoin` relation that
gates `Transition.join`. The freshness conjunct is inside `admitsJoin`, so the
nonce check is threaded by construction — a replayed token (`tok.nonce ∈
s.consumedNonces`) makes both the Bool `false` and the Prop unprovable. -/
theorem decideAdmitsJoin_agrees (s : DiscoveryState) (tok : Token) (tofuBootstrap : Bool) :
    decideAdmitsJoin s tok tofuBootstrap = true ↔ admitsJoin s tok tofuBootstrap := by
  unfold decideAdmitsJoin
  exact decide_eq_true_iff

end PeerRegistryDiscovery
