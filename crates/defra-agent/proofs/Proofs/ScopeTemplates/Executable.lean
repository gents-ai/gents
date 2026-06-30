import Proofs.ScopeTemplates.State

/-!
# Scope Templates — Executable Contract

Small executable vocabulary consumed by the Rust conformance bridge. Mirrors the
`Executable.lean` round-trip pattern (e.g. `PairingReconcile`,
`PeerRegistryDiscovery`): stringly-typed `Delivery` / `Scope` kinds with
`toContract`/`fromContract?` round-trips, matching the Rust `Delivery` and
`Scope` enums in `templates.rs`.
-/

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

/-- Coarse scope-kind vocabulary (the field payload of `peerDid` is carried
separately in the resolved filter; the contract distinguishes the *kind*). -/
inductive ScopeKind where
  | peerDid
  | unscoped
  | perCollection
  deriving DecidableEq, Repr

namespace ScopeKind

/-- Project a `Scope` to its contract kind. -/
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

/-- The projection agrees with the contract round-trip on any scope. -/
theorem ofScope_roundtrips (s : Scope) :
    fromContract? (ofScope s).toContract = some (ofScope s) := by
  cases s <;> rfl

end ScopeKind

end ScopeTemplates
