import Proofs.GoalAutomation.WakeReceipt
import Lean

namespace Conformance.GoalWakeReceiptContracts

open Lean GoalAutomation.RequestAssignment GoalAutomation.WakeReceipt

structure Prior where
  claim : Claim
  state : RequestState
  createGoal : Bool := false
  deriving Repr

structure Case where
  name : String
  claim : Claim
  notifications : List Notification
  prior : List Prior := []
  supersededOrigins : Bool := false

def cases : List Case :=
  let a := QueueScope.referenced 1 100
  let b := QueueScope.referenced 2 200
  let otherRoot := QueueScope.referenced 1 200
  [ ⟨"separate_goal_assignments", ⟨900,a,3⟩, [⟨1,1,a⟩,⟨2,2,b⟩,⟨3,3,.unassigned⟩], [], false⟩
  , ⟨"same_goal_new_root", ⟨900,otherRoot,2⟩, [⟨1,1,a⟩,⟨2,2,otherRoot⟩], [], false⟩
  , ⟨"unassigned_excludes_assigned", ⟨900,.unassigned,2⟩, [⟨1,1,.unassigned⟩,⟨2,2,a⟩], [], false⟩
  , ⟨"interleaved_completed_assignments", ⟨900,a,3⟩, [⟨1,1,a⟩,⟨2,2,b⟩,⟨3,3,a⟩],
      [⟨⟨800,a,1⟩,.completed,false⟩,⟨⟨801,b,2⟩,.completed,false⟩], false⟩
  , ⟨"failed_attempt_remains_input", ⟨900,a,2⟩, [⟨1,1,a⟩,⟨2,2,a⟩], [⟨⟨800,a,1⟩,.failed,false⟩], false⟩
  , ⟨"completed_receipt_after_goal_creation", ⟨900,.unassigned,2⟩,
      [⟨1,1,.unassigned⟩,⟨2,2,.unassigned⟩], [⟨⟨800,.unassigned,1⟩,.completed,true⟩], false⟩
  , ⟨"later_notification_beyond_cutoff", ⟨900,a,1⟩, [⟨1,1,a⟩,⟨2,2,a⟩], [], false⟩
  , ⟨"foreign_completed_receipt", ⟨900,a,2⟩, [⟨1,1,a⟩,⟨2,2,b⟩], [⟨⟨800,b,2⟩,.completed,false⟩], false⟩
  , ⟨"cancelled_attempt_remains_input", ⟨900,a,2⟩, [⟨1,1,a⟩,⟨2,2,a⟩], [⟨⟨800,a,1⟩,.interrupted,false⟩], false⟩
  , ⟨"superseded_same_assignment_origins", ⟨900,a,2⟩, [⟨1,1,a⟩,⟨2,2,b⟩], [], true⟩
  ]

def scopeJson : QueueScope → Json
  | .unassigned => Json.mkObj [("kind",toJson "unassigned")]
  | .unresolved entry => Json.mkObj [("kind",toJson "unresolved"),("entry",toJson entry)]
  | .referenced goal root => Json.mkObj [("kind",toJson "referenced"),("goal",toJson goal),("root",toJson root)]

def claimJson (claim : Claim) : Json := Json.mkObj
  [("wake",toJson claim.wake),("scope",scopeJson claim.scope),("cutoff",toJson claim.cutoff)]

def caseJson (c : Case) : Json :=
  let receipts := c.prior.map fun prior => Receipt.mk (capture prior.claim c.notifications) prior.state
  Json.mkObj [("name",toJson c.name),("claim",claimJson c.claim),
    ("notifications",toJson (c.notifications.map fun row => Json.mkObj
      [("message",toJson row.message),("sequence",toJson row.sequence),("scope",scopeJson row.scope)])),
    ("prior",toJson (c.prior.map fun prior => Json.mkObj [("claim",claimJson prior.claim),
      ("state",toJson prior.state.toDefraDB),("create_goal",toJson prior.createGoal)])),
    ("superseded_origins",toJson c.supersededOrigins),
    ("captured",toJson ((capture c.claim c.notifications).attemptedBindings.map (·.messageId))),
    ("current",toJson ((current c.claim c.notifications receipts).map (·.messageId)))]

def casesJson : String := (toJson (cases.map caseJson)).compress

example : cases.length = 10 := by decide

end Conformance.GoalWakeReceiptContracts
