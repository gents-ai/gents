import Proofs.Session.Enrollment.CapturedContext
import Lean

namespace Enrollment

deriving instance Lean.ToJson for CapturedContextKind, CapturedContextSource, CapturedPosition,
  CapturedRange, CapturedContext

end Enrollment
