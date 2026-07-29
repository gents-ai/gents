/-!
# Background Completion Delivery

A background child completion updates the durable parent bridge and appends a
notification to the parent transcript. Neither step creates an `AgentRequest`.
The next model turn, if any, must therefore be caused by a user-authored
request (or by an independently modelled subagent relationship).
-/

namespace Subagent
namespace CompletionDelivery

inductive DeliveryStage where
  | waiting
  | projected
  | notified
  deriving DecidableEq

structure DeliveryState where
  stage : DeliveryStage
  agentRequestCount : Nat

inductive Transition : DeliveryState → DeliveryState → Prop where
  | project {pre post : DeliveryState}
      (h_pre : pre.stage = .waiting)
      (h_post : post.stage = .projected)
      (h_requests : post.agentRequestCount = pre.agentRequestCount) :
      Transition pre post
  | appendNotification {pre post : DeliveryState}
      (h_pre : pre.stage = .projected)
      (h_post : post.stage = .notified)
      (h_requests : post.agentRequestCount = pre.agentRequestCount) :
      Transition pre post

inductive Trace : DeliveryState → DeliveryState → Prop where
  | refl {state : DeliveryState} : Trace state state
  | step {pre next post : DeliveryState} :
      Transition pre next → Trace next post → Trace pre post

def Initial (state : DeliveryState) : Prop :=
  state.stage = .waiting ∧ state.agentRequestCount = 0

theorem transition_preserves_agent_request_count
    {pre post : DeliveryState}
    (h : Transition pre post) :
    post.agentRequestCount = pre.agentRequestCount := by
  cases h with
  | project _ _ h_requests => exact h_requests
  | appendNotification _ _ h_requests => exact h_requests

theorem trace_preserves_agent_request_count
    {pre post : DeliveryState}
    (h : Trace pre post) :
    post.agentRequestCount = pre.agentRequestCount := by
  induction h with
  | refl => rfl
  | step h_transition _ h_trace =>
      rw [h_trace, transition_preserves_agent_request_count h_transition]

/-- Projection and notification cannot manufacture an agent request. -/
theorem no_synthetic_agent_request
    {pre post : DeliveryState}
    (h_initial : Initial pre)
    (h_trace : Trace pre post) :
    post.agentRequestCount = 0 := by
  rw [trace_preserves_agent_request_count h_trace]
  exact h_initial.2

end CompletionDelivery
end Subagent
