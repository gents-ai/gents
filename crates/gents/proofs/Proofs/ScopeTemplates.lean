import Proofs.ScopeTemplates.State
import Proofs.ScopeTemplates.Derivation
import Proofs.ScopeTemplates.Executable

/-!
# Scope Templates Model

Barrel import for the pure scope-template resolution model that sits beside the
`PairingReconcile` reconciler (the `PeerRegistryDiscovery` pattern): template
catalog state, the deterministic + catalog-total `resolveTemplate` derivation,
the pure `scopeFilter` case-split, and the executable conformance contract for
the `Delivery`/`Scope` vocabulary.
-/
