import Proofs.Store.Identity.SessionInterrupt

namespace Identity.SessionInterruptProposal

open Identity.Household

/-- A proposal, not in `Household.policy`: an `interrupter` relation on the
marker, so an actor with no update on the session may interrupt it. It would
name the Agent whose session started this one, granted when that session is
created; such an interrupter reads and bumps the marker without reading the
session. The wired resource (`Identity.SessionInterrupt.resource`) has no such
relation, so who may interrupt a session is who may update it. -/
def resource : ResourceDefinition :=
  { SessionInterrupt.resource with
    relations := SessionInterrupt.resource.relations ++ [⟨"interrupter", ["actor"], []⟩]
    permissions := [permission "read" ["interrupter"] [("parent", "read")],
      permission "update" ["interrupter"] [("parent", "update")],
      permission "delete" [] [("parent", "delete")]] }

/-- Who would hold `interrupter` on a Task-triggered session, which the
starting session's Agent does not cover: open, one of these. -/
inductive TaskTriggeredInterrupter where
  /-- Every Agent of the session's Node: `interrupter` admits
  `agents->executor` of that Node's Agents. -/
  | sameNodeAgents
  /-- Only the Task's own Agent, already the session's working Agent. -/
  | workingAgentOnly
  deriving DecidableEq, Repr

example : (policy.find? (·.name == resource.name)).map (·.relations.map (·.name)) =
    some ["parent"] := by decide

end Identity.SessionInterruptProposal
