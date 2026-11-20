import Proofs.Store.DocumentAcpChoice
import Proofs.Conformance.Family

namespace DocumentAcpChoice

deriving instance Lean.ToJson for Provider, Store, Admission, Reopening, Refusal, Reopened

attribute [conformance_function] admit reopened

end DocumentAcpChoice
