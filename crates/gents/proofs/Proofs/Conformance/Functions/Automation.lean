import Proofs.Automation.Mailbox.State
import Proofs.Automation.RuntimeReconcile.State
import Proofs.Automation.GoalAutomation.ReadinessGate
import Proofs.Conformance.Family

namespace Mailbox
deriving instance Lean.ToJson for Status
end Mailbox

namespace RuntimeState
deriving instance Lean.ToJson for ConfigurationAdmission
end RuntimeState

namespace Goals
deriving instance Lean.ToJson for Decision
end Goals

attribute [conformance_function]
  Mailbox.Status.terminal
  RuntimeState.configurationAdmission
  GoalAutomation.ReadinessGate.publishes
