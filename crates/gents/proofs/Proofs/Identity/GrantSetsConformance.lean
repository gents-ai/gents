import Proofs.Identity.GrantSets
import Lean

namespace Identity.GrantSetsConformance
open Lean Identity.GrantSets

def setCasesJson : String := (toJson (all.map fun set =>
  Json.mkObj [("set", toJson set.name), ("permissions", toJson set.permissions),
    ("native_admin", toJson set.nativeAdmin)])).compress

def recipients : List Recipient := [.initUser, .enrolledUser, .defaultAgent, .agent]

def recipientCasesJson : String := (toJson (recipients.map fun r =>
  Json.mkObj [("recipient", toJson r.name),
    ("direct", toJson ((directSets r).map GrantSet.name)),
    ("projected", toJson ((projectedSets r).map GrantSet.name))])).compress

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
   ("agent-refused-held", .agent, grant "read-document"),
   ("agent-refused-revoke", .agent, revoke "read-document")]

def delegationCasesJson : String := (toJson (delegationCases.map fun (name, r, c) =>
  Json.mkObj [("name", toJson name), ("holder", toJson r.name),
    ("relation", toJson (relationName c.relation)), ("delete", toJson c.delete),
    ("expected", toJson (admits (holderOf (receives r)) c))])).compress

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

def trace (actor : Holder) (installed : List String) : List Step → List Json
  | [] => []
  | step :: rest =>
      let result := projectBaseline actor step.after installed
      let next := result.getD installed
      Json.mkObj [("action", toJson step.name), ("committed", toJson result.isSome),
        ("installed", toJson next)] :: trace actor next rest

def baselineCases : List (String × String × Holder × List Step) :=
  let lifecycle := [Step.publish, .disable, .enable, .delete]
  [("enrolled-user-lifecycle", Recipient.enrolledUser.name, holderOf (receives .enrolledUser), lifecycle),
   ("household-owner-lifecycle", Recipient.initUser.name, holderOf (receives .initUser), lifecycle),
   ("default-agent-publishes", Recipient.defaultAgent.name, holderOf (receives .defaultAgent), [.publish]),
   ("baseline-agent-cannot-publish", Recipient.agent.name, holderOf (receives .agent), [.publish])]

def baselineCasesJson : String := (toJson (baselineCases.map fun (name, actor, holder, steps) =>
  Json.mkObj [("name", toJson name), ("actor", toJson actor),
    ("steps", toJson (trace holder [] steps))])).compress

end Identity.GrantSetsConformance
