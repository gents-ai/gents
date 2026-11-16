import Proofs.Store.Identity.UserDelegation
import Proofs.Conformance.Family

namespace Identity.UserDelegation

deriving instance Lean.ToJson for Carried, Verdict, Author, Admission, Credential

attribute [conformance_function] admit credential

end Identity.UserDelegation
