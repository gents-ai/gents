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
  retryParent : Option Nat := none

def cases : List Case :=
  let a := QueueScope.referenced 1 100
  let b := QueueScope.referenced 2 200
  let otherRoot := QueueScope.referenced 1 200
  [ ⟨"separate_goal_assignments", ⟨900,a,3⟩, [⟨1,1,a⟩,⟨2,2,b⟩,⟨3,3,.unassigned⟩], [], false, none⟩
  , ⟨"same_goal_new_root", ⟨900,otherRoot,2⟩, [⟨1,1,a⟩,⟨2,2,otherRoot⟩], [], false, none⟩
  , ⟨"unassigned_excludes_assigned", ⟨900,.unassigned,2⟩, [⟨1,1,.unassigned⟩,⟨2,2,a⟩], [], false, none⟩
  , ⟨"interleaved_completed_assignments", ⟨900,a,3⟩, [⟨1,1,a⟩,⟨2,2,b⟩,⟨3,3,a⟩],
      [⟨⟨800,a,1⟩,.completed,false⟩,⟨⟨801,b,2⟩,.completed,false⟩], false, none⟩
  , ⟨"failed_attempt_remains_input", ⟨900,a,2⟩, [⟨1,1,a⟩,⟨2,2,a⟩], [⟨⟨800,a,1⟩,.failed,false⟩], false, none⟩
  , ⟨"completed_receipt_after_goal_creation", ⟨900,.unassigned,2⟩,
      [⟨1,1,.unassigned⟩,⟨2,2,.unassigned⟩], [⟨⟨800,.unassigned,1⟩,.completed,true⟩], false, none⟩
  , ⟨"later_notification_beyond_cutoff", ⟨900,a,1⟩, [⟨1,1,a⟩,⟨2,2,a⟩], [], false, none⟩
  , ⟨"foreign_completed_receipt", ⟨900,a,2⟩, [⟨1,1,a⟩,⟨2,2,b⟩], [⟨⟨800,b,2⟩,.completed,false⟩], false, none⟩
  , ⟨"cancelled_attempt_remains_input", ⟨900,a,2⟩, [⟨1,1,a⟩,⟨2,2,a⟩], [⟨⟨800,a,1⟩,.interrupted,false⟩], false, none⟩
  , ⟨"superseded_same_assignment_origins", ⟨900,a,2⟩, [⟨1,1,a⟩,⟨2,2,b⟩], [], true, none⟩
  , ⟨"failed_goal_creation_retry_keeps_exact_snapshot", ⟨900,.referenced 3 800,1⟩,
      [⟨1,1,.unassigned⟩], [⟨⟨800,.unassigned,1⟩,.failed,true⟩], false, some 800⟩
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
    ("retry_parent",toJson c.retryParent),
    ("captured",toJson ((captureWithRetry c.claim c.notifications receipts c.retryParent).attemptedBindings.map (·.messageId))),
    ("current",toJson ((current c.claim c.notifications receipts c.retryParent).map (·.messageId)))]

def casesJson : String := (toJson (cases.map caseJson)).compress

example : cases.length = 11 := by decide

end Conformance.GoalWakeReceiptContracts
