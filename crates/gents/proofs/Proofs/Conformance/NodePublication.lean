import Proofs.Store.Identity.NodePublication
import Proofs.Conformance.Family
import Proofs.Conformance.UserDelegation

namespace Identity.NodePublication

deriving instance Lean.ToJson for Publication

attribute [conformance_function] publication

end Identity.NodePublication
