import Proofs.P2P.Operations
import Proofs.Conformance.Family

namespace PeerOperations

deriving instance Lean.ToJson for RegistryRefusal, RegistryRecovery

attribute [conformance_function]
  peerOnline
  pairingReplicating
  pairingSubscribed
  pairingConnected
  RegistryRefusal.recovery

end PeerOperations
