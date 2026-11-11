import Proofs.Store.Identity.GrantSets
import Proofs.Conformance.Family

namespace Identity.GrantSets

deriving instance Lean.ToJson for GrantSet

attribute [conformance_vocabulary] GrantSet.name

attribute [conformance_function] GrantSet.nativeAdmin

end Identity.GrantSets
