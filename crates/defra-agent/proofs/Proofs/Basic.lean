/-!
# Basic Definitions

Shared types and utilities used across all three layers of the
ideal agent state machine.

## Modeling boundary: `Nat`-typed IDs and `Time` (#558)

`Time`, `SessionId`, `RequestId`, `BehaviorId`, and the other `abbrev … := Nat`
aliases throughout the proofs are **deliberate abstractions**. The models need
decidable equality and ordering for lifecycle/ordering arguments; they do not
model:

- wall-clock skew or non-monotonic host clocks
- UUID/string parse or serialize failures
- ID-namespace collisions (two real identifiers collapsed to the same `Nat`)
- cross-node identity mismatches (`AgentDid` / `PeerId` / `RequestId` across
  deployments)

Cross-node identity uniqueness and collision-freedom are **not** claimed here.
Distributed identity/membership obligations live at the TLA+ boundary
(`tla/`, e.g. reverse-pairing / transport) and in Rust integration tests, not
in these per-node lifecycle machines. Recorded as
`boundary.model.nat-typed-ids-time` in `Proofs/Conformance/Boundaries.lean`.
-/

/-- Abstract time as natural numbers. We only need ordering, not real clocks.
    See module docstring: wall-clock skew is outside the model (#558). -/
abbrev Time := Nat

/-- A session identifier. Opaque — we only need equality.
    Real session keys may be strings/UUIDs; collision-freedom is a substrate
    assumption, not proven here (#558). -/
abbrev SessionId := Nat

/-- A request identifier within a session.
    Cross-node request-id uniqueness is not modeled (#558). -/
abbrev RequestId := Nat

/-- A behavior identifier bound to a session. -/
abbrev BehaviorId := Nat

/-- Predicate: a state type has terminal states. -/
class HasTerminal (α : Type) where
  isTerminal : α → Prop
  isTerminal_dec : DecidablePred isTerminal

export HasTerminal (isTerminal)

instance {α : Type} [HasTerminal α] : DecidablePred (isTerminal (α := α)) :=
  HasTerminal.isTerminal_dec
