import Proofs.Automation.Mailbox.Operations
import Proofs.Automation.Mailbox.State
import Proofs.Automation.RuntimeReconcile.State
import Proofs.Automation.Triggers.ManualRun
import Proofs.Automation.GoalAutomation.ReadinessGate
import Proofs.Automation.GoalAutomation.Operations
import Proofs.Automation.Triggers.MaterializedLineage
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

namespace GoalAutomation.Operations
deriving instance Lean.ToJson for Actor, Session, Existing, Claim, StartDecision, Target,
  FieldChange, Reason, SetDecision
end GoalAutomation.Operations

deriving instance Lean.ToJson for TriggerKind
deriving instance Lean.ToJson for Enrollment.AgentRequestAdmissionKind

namespace MaterializedLineage
deriving instance Lean.ToJson for ProvenanceRefusal, Fire, Materialized

@[conformance_family]
def materialization : Conformance.Family Fire Materialized where
  inputs := fires
  model := materialize
end MaterializedLineage

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
  GoalAutomation.Operations.startDecision
  GoalAutomation.Operations.setDecision
