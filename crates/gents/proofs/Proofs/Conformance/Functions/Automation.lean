import Proofs.Automation.Mailbox.State
import Proofs.Automation.RuntimeReconcile.State
import Proofs.Automation.GoalAutomation.ReadinessGate
import Proofs.Automation.GoalAutomation.UpdateGoal
import Proofs.Conformance.Family

namespace Mailbox
deriving instance Lean.ToJson for Status
end Mailbox

namespace RuntimeState
deriving instance Lean.ToJson for ConfigurationAdmission
end RuntimeState

namespace Goals
deriving instance Lean.ToJson for Decision
deriving instance Lean.ToJson for Status
end Goals

namespace GoalAutomation.UpdateGoal
deriving instance Lean.ToJson for RequestedStatus, ReasonArgument, Route
end GoalAutomation.UpdateGoal

attribute [conformance_function]
  Mailbox.Status.terminal
  RuntimeState.configurationAdmission
  GoalAutomation.ReadinessGate.publishes
  GoalAutomation.UpdateGoal.route
