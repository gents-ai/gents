import Proofs.Runtime.BackendHealth.State
import Proofs.Runtime.RuntimeReconcile.MetricsScrapeAccess
import Proofs.Runtime.RuntimeReconcile.UpdateHandoff
import Proofs.Runtime.RouterDispatch
import Proofs.Conformance.Family

namespace Proofs.BackendHealth
deriving instance Lean.ToJson for HealthState
end Proofs.BackendHealth

namespace RuntimeReconcile.MetricsScrapeAccess
deriving instance Lean.ToJson for Bind
end RuntimeReconcile.MetricsScrapeAccess

namespace RuntimeReconcile.MetricsScrapeAccess
deriving instance Lean.ToJson for Decision
end RuntimeReconcile.MetricsScrapeAccess

namespace RuntimeReconcile.UpdateHandoff
deriving instance Lean.ToJson for Phase
end RuntimeReconcile.UpdateHandoff

namespace RouterDispatch
deriving instance Lean.ToJson for WaitEvent, Destination, BlockedDispatch
end RouterDispatch

attribute [conformance_function]
  RouterDispatch.blockedDispatch
  Proofs.BackendHealth.HealthState.blocksRouting
  RuntimeReconcile.MetricsScrapeAccess.decide
  RuntimeReconcile.MetricsScrapeAccess.bindAllowed
