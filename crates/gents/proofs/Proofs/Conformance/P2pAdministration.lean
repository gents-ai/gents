import Proofs.Tools.P2pAdministration
import Proofs.Conformance.Family

namespace P2pAdministration
deriving instance Lean.ToJson for Actor
deriving instance Lean.ToJson for Command
deriving instance Lean.ToJson for Peer
deriving instance Lean.ToJson for Outcome
end P2pAdministration

attribute [conformance_function] P2pAdministration.admit
