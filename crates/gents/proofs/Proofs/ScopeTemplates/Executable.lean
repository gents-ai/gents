import Proofs.ScopeTemplates.State

namespace ScopeTemplates

def domainName : String := "ScopeTemplates"

namespace Delivery

def toContract : Delivery → String
  | .push => "push"
  | .replicate => "replicate"

def fromContract? : String → Option Delivery
  | "push" => some .push
  | "replicate" => some .replicate
  | _ => none

theorem fromContract_toContract (d : Delivery) :
    fromContract? d.toContract = some d := by
  cases d <;> rfl

end Delivery

inductive ScopeKind where
  | peerDid
  | unscoped
  | perCollection
  deriving DecidableEq, Repr

namespace ScopeKind

def ofScope : Scope → ScopeKind
  | .peerDid _ => .peerDid
  | .unscoped => .unscoped
  | .perCollection _ => .perCollection

def toContract : ScopeKind → String
  | .peerDid => "peerDid"
  | .unscoped => "unscoped"
  | .perCollection => "perCollection"

def fromContract? : String → Option ScopeKind
  | "peerDid" => some .peerDid
  | "unscoped" => some .unscoped
  | "perCollection" => some .perCollection
  | _ => none

theorem fromContract_toContract (k : ScopeKind) :
    fromContract? k.toContract = some k := by
  cases k <;> rfl

theorem ofScope_roundtrips (s : Scope) :
    fromContract? (ofScope s).toContract = some (ofScope s) := by
  cases s <;> rfl

end ScopeKind

end ScopeTemplates
