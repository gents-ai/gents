import Proofs.Store.Identity.GrantSets
import Proofs.Store.SchemaInstall
import Proofs.Conformance.Family

namespace Identity.GrantSets

deriving instance Lean.ToJson for GrantSet

attribute [conformance_vocabulary] GrantSet.name

attribute [conformance_function] GrantSet.nativeAdmin GrantSet.permissions

end Identity.GrantSets

namespace SchemaInstall

deriving instance Lean.ToJson for Live, Decision

attribute [conformance_function] decide requestAdmitted

end SchemaInstall
