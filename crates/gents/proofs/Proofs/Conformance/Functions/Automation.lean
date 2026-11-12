import Proofs.Automation.Mailbox.Operations
import Proofs.Automation.Mailbox.State
import Proofs.Automation.RuntimeReconcile.State
import Proofs.Automation.Triggers.ManualRun
import Proofs.Automation.GoalAutomation.ReadinessGate
import Proofs.Automation.GoalAutomation.UpdateGoal
import Proofs.Conformance.Family

namespace Mailbox
deriving instance Lean.ToJson for Status, Handling
end Mailbox

namespace Mailbox.Operations
deriving instance Lean.ToJson for ListScope, DismissDecision, ReplyKind, ReplyDecision
end Mailbox.Operations

namespace Triggers.ManualRun
deriving instance Lean.ToJson for Presence, RunDecision, Matches, ScheduleDecision, TitleKind
end Triggers.ManualRun

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
  Mailbox.Operations.serves
  Mailbox.Operations.dismissDecision
  Mailbox.Operations.replyDecision
  RuntimeState.configurationAdmission
  Triggers.ManualRun.runDecision
  Triggers.ManualRun.scheduleDecision
  Triggers.ManualRun.sessionTitle
  GoalAutomation.ReadinessGate.publishes
  GoalAutomation.UpdateGoal.route
