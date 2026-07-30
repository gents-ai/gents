abbrev Time := Nat

abbrev SessionId := Nat

abbrev RequestId := Nat

abbrev BehaviorId := Nat

class HasTerminal (α : Type) where
  isTerminal : α → Prop
  isTerminal_dec : DecidablePred isTerminal

export HasTerminal (isTerminal)

instance {α : Type} [HasTerminal α] : DecidablePred (isTerminal (α := α)) :=
  HasTerminal.isTerminal_dec
