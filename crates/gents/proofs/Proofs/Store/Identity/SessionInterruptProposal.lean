import Proofs.Store.Identity.Household

namespace Identity.SessionInterruptProposal

open Identity.Household

/-- The interrupt marker's resource: one `AgentSessionInterrupt` per session,
its parent that session. It is created with the session, in the transaction
that creates the session and as the actor that creates it, so an interrupter
always updates it and never creates it. Whoever reads the session reads it, so
every claim and wake publication of the session can read it by id, and so
does an `interrupter`, which bumps it without reading the session. Whoever
may update the session may bump it (the working Agent; the Agent's and the
Node's administrators), and so may an actor the `interrupter` relation names:
the Agent whose session started this one, granted when that session is
created. Deleting the session deletes it in the same transaction. Not yet in
`Household.policy`. -/
def resource : ResourceDefinition :=
  { name := "session_interrupts"
    relations := [⟨"parent", ["sessions"], []⟩, ⟨"interrupter", ["actor"], []⟩]
    permissions := [permission "read" ["interrupter"] [("parent", "read")],
      permission "update" ["interrupter"] [("parent", "update")],
      permission "delete" [] [("parent", "delete")]] }

/-- Who holds `interrupter` on a Task-triggered session, which the starting
session's Agent does not cover: open, one of these. -/
inductive TaskTriggeredInterrupter where
  /-- Every Agent of the session's Node: `interrupter` admits
  `agents->executor` of that Node's Agents. -/
  | sameNodeAgents
  /-- Only the Task's own Agent, already the session's working Agent. -/
  | workingAgentOnly
  deriving DecidableEq, Repr

example : policy.all (·.name != resource.name) = true := by decide

end Identity.SessionInterruptProposal
