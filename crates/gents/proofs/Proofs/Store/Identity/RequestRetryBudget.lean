import Proofs.Store.Identity.Household

/-!
Who may write a request's pinned successor budget (`AgentRequestRetryBudget`,
resource `request_retry_budgets`).

Decision (#4013 review): the budget a request's first claim pins is its own
document, not a field of `AgentRequest`. `delivered_requests` grants `update`
to the working Agent, and ACP authorizes whole documents, so a budget stored
on the request would be one the working Agent could raise after the claim.
The pin is created by the request's Node (the runtime key, which no Agent
tool acts as) right after the first claim commits; the resource grants
nobody `update` or `delete`, so only that owner holds them, and its fields
are `@immutable`. Readers reach it through `parent` (the request's own
readers). ACP1 cannot authenticate who created a document
(`Household.owner_can_attach_unverified_parent`), so readers accept only a pin
the Node signed (`Request.PinnedRetryBudget.readPin`).
-/
namespace Identity.RequestRetryBudget

open Identity.Household

def pinned : State :=
  { objects := [⟨"agents", "agent-doc", "user"⟩, ⟨"sessions", "session", "user"⟩,
      ⟨"delivered_requests", "request", "user"⟩,
      ⟨"request_retry_budgets", "pin", "node"⟩],
    relations := [⟨"agents", "agent-doc", "administrator", .actor "user"⟩,
      ⟨"sessions", "session", "parent", .object "agents" "agent-doc"⟩,
      ⟨"sessions", "session", "working_agent", .actor "agent"⟩,
      ⟨"delivered_requests", "request", "parent", .object "agents" "agent-doc"⟩,
      ⟨"delivered_requests", "request", "session", .object "sessions" "session"⟩,
      ⟨"delivered_requests", "request", "working_agent", .actor "agent"⟩,
      ⟨"request_retry_budgets", "pin", "parent", .object "delivered_requests" "request"⟩],
    enrolled := [] }

/-- The working Agent may update its request, but not the Node's pin of it. -/
theorem working_agent_cannot_change_the_pin :
    (allowed 6 pinned "agent" "delivered_requests" "request" "update",
     allowed 6 pinned "agent" "request_retry_budgets" "pin" "read",
     allowed 6 pinned "agent" "request_retry_budgets" "pin" "update",
     allowed 6 pinned "agent" "request_retry_budgets" "pin" "delete") =
      (true, true, false, false) := by native_decide

/-- The request's owner and administrators read the pin and cannot change it
either; only its creator, the Node, holds every permission. -/
theorem only_the_node_owns_the_pin :
    (allowed 6 pinned "user" "request_retry_budgets" "pin" "read",
     allowed 6 pinned "user" "request_retry_budgets" "pin" "update",
     allowed 6 pinned "user" "request_retry_budgets" "pin" "delete",
     allowed 6 pinned "node" "request_retry_budgets" "pin" "update",
     allowed 6 pinned "stranger" "request_retry_budgets" "pin" "read") =
      (true, false, false, true, false) := by native_decide

/-- A pin the working Agent creates itself is one it owns and may change:
ACP alone cannot tell it from the Node's, which is why readers accept only a
Node-signed pin. -/
theorem an_agent_created_pin_is_the_agents :
    allowed 6 (directCreate pinned "agent" "request_retry_budgets" "forged" true true)
      "agent" "request_retry_budgets" "forged" "update" = true := by native_decide

end Identity.RequestRetryBudget
