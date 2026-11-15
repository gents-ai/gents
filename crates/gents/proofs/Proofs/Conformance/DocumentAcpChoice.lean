import Proofs.Store.DocumentAcpChoice
import Proofs.Conformance.Family

namespace DocumentAcpChoice

deriving instance Lean.ToJson for Provider, Store, Admission, Entry, Reopening, Refusal, Reopened

attribute [conformance_function] admit enter reopened

end DocumentAcpChoice
