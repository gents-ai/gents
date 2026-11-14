import Proofs.Store.Identity.GrantSets
import Lean

namespace Identity.GrantSetsConformance
open Lean Identity.GrantSets

def recipients : List Recipient := [.initUser, .enrolledUser, .defaultAgent, .agent]

def relationName : Relation → String
  | .admin => "admin"
  | .permission p => p

def delegationCases : List (String × Recipient × Change) :=
  let grant (p : String) : Change := ⟨.permission p, false, true, false⟩
  let revoke (p : String) : Change := ⟨.permission p, true, true, false⟩
  [("enrolled-grants-held", .enrolledUser, grant "delete-document"),
   ("enrolled-revokes-held", .enrolledUser, revoke "read-document"),
   ("enrolled-refused-schema", .enrolledUser, grant "patch-collection"),
   ("enrolled-refused-admin-class", .enrolledUser, grant "add-nac-relation"),
   ("enrolled-refused-admin", .enrolledUser, ⟨.admin, false, true, false⟩),
   ("owner-grants-schema", .initUser, grant "patch-collection"),
   ("owner-grants-admin-class", .initUser, grant "add-nac-relation"),
   ("default-agent-grants-held", .defaultAgent, grant "update-document"),
   ("default-agent-refused-unheld", .defaultAgent, grant "delete-document"),
   ("default-agent-grants-nac-status", .defaultAgent, grant "get-nac-status"),
   ("enrolled-refused-nac-status", .enrolledUser, grant "get-nac-status"),
   ("agent-refused-held", .agent, grant "read-document"),
   ("agent-refused-revoke", .agent, revoke "read-document")]

inductive Step where
  | publish
  | disable
  | enable
  | delete
  deriving Repr

def Step.name : Step → String
  | .publish => "publish"
  | .disable => "disable"
  | .enable => "enable"
  | .delete => "delete"

def Step.after : Step → AgentObservation
  | .publish | .enable => ⟨true, true, true⟩
  | .disable => ⟨true, false, true⟩
  | .delete => ⟨false, false, true⟩

def baselineCases : List (String × String × Holder × List Step) :=
  let lifecycle := [Step.publish, .disable, .enable, .delete]
  [("enrolled-user-lifecycle", Recipient.enrolledUser.name, holderOf (receives .enrolledUser), lifecycle),
   ("household-owner-lifecycle", Recipient.initUser.name, holderOf (receives .initUser), lifecycle),
   ("default-agent-publishes", Recipient.defaultAgent.name, holderOf (receives .defaultAgent), [.publish]),
   ("baseline-agent-cannot-publish", Recipient.agent.name, holderOf (receives .agent), [.publish])]

end Identity.GrantSetsConformance
