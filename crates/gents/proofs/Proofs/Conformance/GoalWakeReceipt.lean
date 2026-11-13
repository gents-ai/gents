import Proofs.Automation.GoalAutomation.WakeReceipt
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Contracts.Machines.Catalog

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

inductive ScopeKind where
  | unassigned
  | unresolved
  | referenced
  deriving ToJson

structure ScopeWire where
  kind : ScopeKind
  entry : Option Nat
  goal : Option Nat
  root : Option Nat
  deriving ToJson

def ScopeWire.ofModel : QueueScope → ScopeWire
  | .unassigned => ⟨.unassigned, none, none, none⟩
  | .unresolved entry => ⟨.unresolved, some entry, none, none⟩
  | .referenced goal root => ⟨.referenced, none, some goal, some root⟩

def ScopeWire.toModel (scope : ScopeWire) : QueueScope :=
  match scope.kind with
  | .unassigned => .unassigned
  | .unresolved => .unresolved (scope.entry.getD 0)
  | .referenced => .referenced (scope.goal.getD 0) (scope.root.getD 0)

structure ClaimWire where
  wake : Nat
  scope : ScopeWire
  cutoff : Nat
  deriving ToJson

def ClaimWire.ofModel (claim : Claim) : ClaimWire := ⟨claim.wake, .ofModel claim.scope, claim.cutoff⟩

def ClaimWire.toModel (claim : ClaimWire) : Claim := ⟨claim.wake, claim.scope.toModel, claim.cutoff⟩

structure NotificationWire where
  message : Nat
  sequence : Nat
  scope : ScopeWire
  deriving ToJson

structure PriorWire where
  claim : ClaimWire
  state : RequestState
  createGoal : Bool
  deriving ToJson

structure ReceiptInput where
  name : String
  claim : ClaimWire
  notifications : List NotificationWire
  prior : List PriorWire
  supersededOrigins : Bool
  retryParent : Option Nat
  deriving ToJson

structure ReceiptOutput where
  captured : List Nat
  current : List Nat
  deriving ToJson

@[conformance_family]
def receipt : Family ReceiptInput ReceiptOutput where
  inputs := cases.map fun c =>
    { name := c.name, claim := .ofModel c.claim
      notifications := c.notifications.map fun row => ⟨row.message, row.sequence, .ofModel row.scope⟩
      prior := c.prior.map fun prior => ⟨.ofModel prior.claim, prior.state, prior.createGoal⟩
      supersededOrigins := c.supersededOrigins, retryParent := c.retryParent }
  model := fun i =>
    let claim := i.claim.toModel
    let notifications : List Notification :=
      i.notifications.map fun row => ⟨row.message, row.sequence, row.scope.toModel⟩
    let receipts := i.prior.map fun prior =>
      Receipt.mk (capture prior.claim.toModel notifications) prior.state
    { captured := (captureWithRetry claim notifications receipts i.retryParent).attemptedBindings.map
        (·.messageId)
      current := (current claim notifications receipts i.retryParent).map (·.messageId) }

example : cases.length = 11 := by decide

end Conformance.GoalWakeReceiptContracts
