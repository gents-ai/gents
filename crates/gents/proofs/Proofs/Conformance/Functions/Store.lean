import Proofs.Store.Identity.GrantSets
import Proofs.Store.Identity.EnrollmentGrant
import Proofs.Conformance.Family

namespace Identity.GrantSets

deriving instance Lean.ToJson for GrantSet

attribute [conformance_vocabulary] GrantSet.name

attribute [conformance_function] GrantSet.nativeAdmin

end Identity.GrantSets

namespace Identity.EnrollmentGrant

deriving instance Lean.ToJson for ApprovalStep

attribute [conformance_function] afterGrant

end Identity.EnrollmentGrant
