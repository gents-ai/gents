/-!
# Basic Definitions

Shared types and utilities used across all three layers of the
ideal agent state machine.
-/

/-- Abstract time as natural numbers. We only need ordering, not real clocks. -/
abbrev Time := Nat

/-- A session identifier. Opaque — we only need equality. -/
abbrev SessionId := Nat

/-- A request identifier within a session. -/
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
