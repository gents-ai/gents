import Proofs.Store.Identity.NodePublication
import Proofs.Conformance.Family
import Proofs.Conformance.UserDelegation

namespace Identity.NodePublication

deriving instance Lean.ToJson for Publication, Periodic, Loop

attribute [conformance_function] publication periodic

end Identity.NodePublication
