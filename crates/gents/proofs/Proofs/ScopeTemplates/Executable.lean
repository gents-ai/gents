import Proofs.ScopeTemplates.State

namespace ScopeTemplates

def domainName : String := "ScopeTemplates"

namespace Delivery

def toContract : Delivery → String
  | .push => "push"

def fromContract? : String → Option Delivery
  | "push" => some .push
  | _ => none

theorem fromContract_toContract (d : Delivery) :
    fromContract? d.toContract = some d := by
  cases d <;> rfl

end Delivery

inductive ScopeKind where
  | perCollection
  | clientRoute
  deriving DecidableEq, Repr

namespace ScopeKind

def ofScope : Scope → ScopeKind
  | .perCollection _ => .perCollection
  | .clientRoute => .clientRoute

def toContract : ScopeKind → String
  | .perCollection => "perCollection"
  | .clientRoute => "clientRoute"

def fromContract? : String → Option ScopeKind
  | "perCollection" => some .perCollection
  | "clientRoute" => some .clientRoute
  | _ => none

theorem fromContract_toContract (k : ScopeKind) :
    fromContract? k.toContract = some k := by
  cases k <;> rfl

theorem ofScope_roundtrips (s : Scope) :
    fromContract? (ofScope s).toContract = some (ofScope s) := by
  cases s <;> rfl

end ScopeKind

end ScopeTemplates
