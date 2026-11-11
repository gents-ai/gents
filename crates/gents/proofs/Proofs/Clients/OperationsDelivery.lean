import Proofs.Clients.QueuedSteering
import Proofs.Session.Operations

namespace SessionOperations

def DeliveryChoice.toAgent : DeliveryChoice → Option QueuedSteering.AgentDelivery
  | .unspecified => none
  | .queue => some .queue
  | .steer => some .steer

def Delivery.toAgent : Delivery → QueuedSteering.AgentDelivery
  | .queue => .queue
  | .steer => .steer

/-- The agent tool's delivery rule without the legacy interrupt flag is the
shared rule: an unspecified delivery queues and steering happens only when
chosen. -/
theorem resolveDelivery_agrees_with_agent_tool (choice : DeliveryChoice) :
    QueuedSteering.resolveAgentDelivery choice.toAgent none =
      some (resolveDelivery choice).toAgent := by
  cases choice <;> rfl

end SessionOperations
