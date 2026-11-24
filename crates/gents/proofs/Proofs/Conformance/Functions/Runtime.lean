import Proofs.Runtime.BackendHealth.Observation
import Proofs.Runtime.RuntimeReconcile.HttpAccess
import Proofs.Runtime.RuntimeReconcile.MetricsScrapeAccess
import Proofs.Runtime.RuntimeReconcile.UpdateHandoff
import Proofs.Runtime.RouterDispatch
import Proofs.Conformance.Family

namespace Proofs.BackendHealth
deriving instance Lean.ToJson for HealthState, Event, ProbeOutcome
end Proofs.BackendHealth

namespace RuntimeReconcile.HttpAccess
deriving instance Lean.ToJson for Identity
end RuntimeReconcile.HttpAccess

namespace RuntimeReconcile.MetricsScrapeAccess
deriving instance Lean.ToJson for Bind
end RuntimeReconcile.MetricsScrapeAccess

namespace RuntimeReconcile.MetricsScrapeAccess
deriving instance Lean.ToJson for Decision
end RuntimeReconcile.MetricsScrapeAccess

namespace RuntimeReconcile.UpdateHandoff
deriving instance Lean.ToJson for Phase, Registrant, HelperRegistration
end RuntimeReconcile.UpdateHandoff

namespace RouterDispatch
deriving instance Lean.ToJson for WaitEvent, Destination, BlockedDispatch
end RouterDispatch

attribute [conformance_function]
  RouterDispatch.blockedDispatch
  Proofs.BackendHealth.HealthState.blocksRouting
  Proofs.BackendHealth.awaitProbeRequest
  Proofs.BackendHealth.probeEvent
  RuntimeReconcile.MetricsScrapeAccess.decide
  RuntimeReconcile.MetricsScrapeAccess.bindAllowed
  RuntimeReconcile.HttpAccess.mcpQueryReadsAs
  RuntimeReconcile.UpdateHandoff.helperRegistration
  RuntimeReconcile.UpdateHandoff.statusTakesLock
