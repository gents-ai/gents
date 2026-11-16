import Proofs.Store.Identity.UserDelegation
import Proofs.Conformance.Family

namespace Identity.UserDelegation

deriving instance Lean.ToJson for Carrier, Write, Carried, Scope, Verdict, Author, Admission, Credential,
  Reported, Resolution

attribute [conformance_function] admit credential runtimeSignsWithHeldUserKey statusReports resolve

end Identity.UserDelegation
