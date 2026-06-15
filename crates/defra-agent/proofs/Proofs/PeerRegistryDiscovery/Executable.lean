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

/-- Stringly-typed transition kinds emitted by the discovery reconciler. One
constructor per `Transition` constructor (`derive`, `join`, `reciprocalJoin`,
`submitRequest`, `approveRequest`, `revoke`, `operatorWrite`). -/
inductive TransitionKind where
  | derive
  | join
  /-- A reciprocal join (`--reciprocal`): same admission gate as `join`, plus a
  return-leg replicator wired outside the modeled discovery state. Emitted as a
  distinct kind so the Rust bridge cannot route a reciprocal join down a path
  that skips `decideAdmitsJoin` — it shares `join`'s admission decision. -/
  | reciprocalJoin
  | submitRequest
  | approveRequest
  | revoke
  | operatorWrite
  deriving DecidableEq, Repr

namespace TransitionKind

def fromString? : String → Option TransitionKind
  | "derive" => some .derive
  | "join" => some .join
  | "reciprocalJoin" => some .reciprocalJoin
  | "submitRequest" => some .submitRequest
  | "approveRequest" => some .approveRequest
  | "revoke" => some .revoke
  | "operatorWrite" => some .operatorWrite
  | _ => none

def toString : TransitionKind → String
  | .derive => "derive"
  | .join => "join"
  | .reciprocalJoin => "reciprocalJoin"
  | .submitRequest => "submitRequest"
  | .approveRequest => "approveRequest"
  | .revoke => "revoke"
  | .operatorWrite => "operatorWrite"

theorem fromString_toString (k : TransitionKind) :
    fromString? k.toString = some k := by
  cases k <;> rfl

end TransitionKind

/-- Executable coarse transition relation for conformance extraction. A derive
step settles; any membership/request edit (join, reciprocalJoin, submitRequest,
approveRequest, revoke) unsettles the derived view; an operator write leaves the
derived view as-is. -/
def step? : DiscoveryPhase → TransitionKind → Option DiscoveryPhase
  | _, .derive => some .settled
  | _, .join => some .unsettled
  | _, .reciprocalJoin => some .unsettled
  | _, .submitRequest => some .unsettled
  | _, .approveRequest => some .unsettled
  | _, .revoke => some .unsettled
  | phase, .operatorWrite => some phase

/-! ## Executable join-admission decision (mirrors Rust `decide_join_admission`)

The Rust bridge decides whether a join is admissible with a single boolean. This
function is that boolean, and it now threads the single-use nonce check: a join
is admitted iff it is member-signed (or TOFU-bootstrapped) AND the token's nonce
has not already been consumed. `decideAdmitsJoin_agrees` fences it to the
`admitsJoin` Prop that BOTH the `Transition.join` and the `Transition.reciprocalJoin`
constructors require, so the executable decision and the relation can never
diverge on replay — and a reciprocal join takes the identical decision (no
weaker reciprocal path exists). -/
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

/-! ## Executable membership / derivation decisions

The Rust bridge also decides, with single booleans, whether a network's admin
self-attestation verifies, whether a DID is an admitted member, and whether an
endpoint is materializable. These mirror the corresponding Props, each fenced by
an `_agrees` lemma so the executable decision and the relation cannot diverge. -/

/-- The network record's admin self-attestation verifies. -/
def decideValidNetwork (n : Network) : Bool := n.adminSigValid

/-- A DID is an admitted member of `s`'s network. -/
def decideAdmittedMember (did : Did) (s : DiscoveryState) : Bool := decide (admittedMember did s)

/-- An endpoint the derivation materializes. -/
def decideMaterializable (ep : Endpoint) (s : DiscoveryState) : Bool := decide (materializableEndpoint ep s)

/-- `decideValidNetwork` agrees with the `validNetwork` predicate. -/
theorem decideValidNetwork_agrees (n : Network) :
    decideValidNetwork n = true ↔ validNetwork n := by
  unfold decideValidNetwork validNetwork
  exact Iff.rfl

theorem decideAdmittedMember_agrees (did) (s) :
    decideAdmittedMember did s = true ↔ admittedMember did s := by
  unfold decideAdmittedMember; exact decide_eq_true_iff

theorem decideMaterializable_agrees (ep) (s) :
    decideMaterializable ep s = true ↔ materializableEndpoint ep s := by
  unfold decideMaterializable; exact decide_eq_true_iff

end PeerRegistryDiscovery
