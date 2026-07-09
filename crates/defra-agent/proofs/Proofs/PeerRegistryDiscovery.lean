import Proofs.PeerRegistryDiscovery.State
import Proofs.PeerRegistryDiscovery.Transition
import Proofs.PeerRegistryDiscovery.Derivation
import Proofs.PeerRegistryDiscovery.Executable
import Proofs.PeerRegistryDiscovery.NetworkMembership
import Proofs.PeerRegistryDiscovery.ReciprocalConversation
import Proofs.PeerRegistryDiscovery.BearerClaim

/-!
# Peer Registry Discovery Model

Barrel import for the service-discovery derivation that sits above the proven
`PairingReconcile` machine: registry state, the registry→desired derivation,
its idempotence/convergence/ownership/retraction properties, the signed-invite
join guard, and the executable conformance contract.

`NetworkMembership` is the §9 network control-plane layer that **supersedes**
the self-asserted registry: admin-signed `Membership` + member-signed `Endpoint`
materialization, with the five §9 obligations (forged membership never
materialized, signed membership+endpoint is materialized, revocation retracts
exactly, ownership safety, join-request grants nothing). It is the Lean source
of truth the cut-2 SDL collections mirror.
-/
