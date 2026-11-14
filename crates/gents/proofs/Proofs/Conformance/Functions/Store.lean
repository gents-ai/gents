import Proofs.Store.Identity.GrantSets
import Proofs.Store.Identity.EnrollmentGrant
import Proofs.Store.SchemaInstall
import Proofs.Store.AccessClassification
import Proofs.Conformance.Family

namespace Identity.GrantSets

deriving instance Lean.ToJson for GrantSet

attribute [conformance_vocabulary] GrantSet.name

attribute [conformance_function] GrantSet.nativeAdmin GrantSet.permissions

end Identity.GrantSets

namespace Identity.EnrollmentGrant

deriving instance Lean.ToJson for ApprovalStep

attribute [conformance_function] afterGrant

end Identity.EnrollmentGrant

namespace SchemaInstall

deriving instance Lean.ToJson for Live, Decision

attribute [conformance_function] decide requestAdmitted

end SchemaInstall

namespace AccessClassification

deriving instance Lean.ToJson for Classification, Failure, Erasure, StoreWrapper, ViewWrapper

attribute [conformance_function] storeWrapped viewWrapped

end AccessClassification
