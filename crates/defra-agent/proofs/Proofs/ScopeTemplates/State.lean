import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.List.Basic

/-!
# Scope Templates — State

A pure resolution model that sits *beside* the `PairingReconcile` reconciler
(the pattern is `PeerRegistryDiscovery`: a derivation below the machine, not a
new machine). A `ScopeTemplate` is a named pairing intent — a collection set, a
`Scope` (how per-peer document filtering is derived) and a `Delivery` (filtered
push vs. subscribe+replicate).

Mirrors the Rust `crates/defra-agent/src/agent/p2p_reconcile/templates.rs`:
`ScopeTemplate { id, collections, scope, delivery }`, `Scope = PeerDid {field} |
Unscoped`, `Delivery = Push | Replicate`, and `scope_filter`. The catalog is a
static `&[ScopeTemplate]`; here it is a `List Template` over which resolution is
proven deterministic and total.

`@immutable`/DAG-completeness is upstream's obligation (#1033); not modeled here.
-/

namespace ScopeTemplates

abbrev TemplateId := String
abbrev Did := String

/-- Delivery mode. `Push` = filtered replicator only (no subscription);
`Replicate` = subscribe + unfiltered replicator (whole-collection). -/
inductive Delivery where
  | push
  | replicate
  deriving DecidableEq, Repr

/-- Scoping policy. `PeerDid f` filters each collection on field `f` equal to the
peer's DID; `Unscoped` applies no filter. -/
inductive Scope where
  | peerDid (field : String)
  | unscoped
  deriving DecidableEq, Repr

/-- The scope filter key resolved from a scope against a concrete peer DID.
Byte-identical in shape to the predicate part of Rust
`FilterPredicate { field, value }`. -/
structure ScopeFilterKey where
  field : String
  value : Did
  deriving DecidableEq, Repr

/-- One per-collection filter resolved from a scope against a concrete peer DID.
Mirrors Rust `PairingFilters` entries: map key = `collection`, value =
`FilterPredicate { field, value }`. -/
structure CollectionScopeFilter where
  collection : String
  field : String
  value : Did
  deriving DecidableEq, Repr

/-- A named pairing intent. -/
structure Template where
  id : TemplateId
  collections : Finset String
  scope : Scope
  delivery : Delivery
  deriving DecidableEq

/-- The catalog: an ordered list of templates. Mirrors the static Rust
`&[ScopeTemplate]`. Resolution is `List.find?` by id, matching
`resolve_template`'s `iter().find(|t| t.id == id)`. -/
abbrev Catalog := List Template

end ScopeTemplates
