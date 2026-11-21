import Proofs.Store.Identity.GenesisReadAccess
import Proofs.Conformance.Family

namespace Identity.GenesisReadAccess

deriving instance Lean.ToJson for Collection, Resource, Binding, Access

attribute [conformance_function] receiverAccess

end Identity.GenesisReadAccess
