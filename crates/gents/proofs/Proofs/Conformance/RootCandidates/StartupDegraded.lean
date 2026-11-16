import Proofs.Runtime.StartupDegraded
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Pack

attribute [conformance_function] Runtime.StartupDegraded.allowsStartup
  Runtime.StartupDegraded.degradesNode Runtime.StartupDegraded.initAgentReason
  Runtime.StartupDegraded.enabledAgentReason
