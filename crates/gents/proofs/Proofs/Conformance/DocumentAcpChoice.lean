import Proofs.Store.DocumentAcpChoice
import Proofs.Conformance.Family

namespace DocumentAcpChoice

deriving instance Lean.ToJson for Provider, Store, Admission

attribute [conformance_function] admit

end DocumentAcpChoice
