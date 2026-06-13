import Proofs.PeerRegistryDiscovery.State
import Proofs.PeerRegistryDiscovery.Transition
import Proofs.PeerRegistryDiscovery.Derivation
import Proofs.PeerRegistryDiscovery.Executable

/-!
# Peer Registry Discovery Model

Barrel import for the service-discovery derivation that sits above the proven
`PairingReconcile` machine: registry state, the registry→desired derivation,
its idempotence/convergence/ownership/retraction properties, the signed-invite
join guard, and the executable conformance contract.
-/
