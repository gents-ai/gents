import Proofs.Session.InputEdit
import Proofs.Session.ManagementCases

namespace SessionQueue.InputEditCases
open InputEdit ManagementCases FoldCases

def authority : Authority := ⟨true,true,true,true,true,true,true⟩
/-- A local command: no peer route and the requester is the node. -/
def command : Command := ⟨10,20,some 1,false,1,10,[2],0,1,[replacement 3 2]⟩
def remote : Command := {command with hasPeer := true}
def remoteAuthority : Authority := {authority with requesterIsNode := false}
def state : State := ⟨initial [pending 2],[]⟩

/-- Earlier events that produced a case's starting state. -/
inductive Step where
  | apply (now : Nat) (authority : Authority) (command : Command)
  | loseReceipts
  /-- Later commands target a fresh queue in another session of the same node. -/
  | otherSession (session : Nat)

def Step.run (s : State) : Step → State
  | .apply now a c => (InputEdit.apply s.queue.scope now a c s).1
  | .loseReceipts => { s with receipts := [] }
  | .otherSession session =>
      { s with queue := { state.queue with scope := { state.queue.scope with session } } }

structure Case where
  name : String
  history : List Step := []
  command : Command
  authority : Authority
  now : Nat

def Case.before (c : Case) : State := c.history.foldl Step.run state

def applied : List Step := [.apply 2 authority command]

def cases : List Case :=
  [ ⟨"remote_applied",[],remote,remoteAuthority,2⟩
  , ⟨"revoked_enrollment_rejected",[],remote,{remoteAuthority with enrollmentFresh := false},2⟩
  , ⟨"missing_route_rejected",[],remote,{remoteAuthority with routeApplied := false},2⟩
  , ⟨"local_self_applied",[],command,authority,2⟩
  , ⟨"local_foreign_rejected",[],command,{authority with requesterIsNode := false},2⟩
  , ⟨"expired_rejected",[],command,authority,10⟩
  , ⟨"future_issuance_deferred",[],command,authority,0⟩
  , ⟨"signature_rejected",[],command,{authority with signatureValid := false},2⟩
  , ⟨"replacement_signature_rejected",[],command,{authority with replacementsValid := false},2⟩
  , ⟨"foreign_session_rejected",[],command,{authority with sessionOwned := false},2⟩
  , ⟨"stale_queue_rejected",[],{command with expected := []},authority,2⟩
  , ⟨"applied_effects_without_receipt_after_expiry",applied ++ [.loseReceipts],command,authority,100⟩
  , ⟨"exact_replay_after_expiry",applied,command,authority,100⟩
  , ⟨"command_identity_collision",applied,{command with digest := 21},authority,2⟩
  , ⟨"simultaneous_command_identity_collision",[],command,
      {authority with commandIdentityUnique := false},2⟩
  , ⟨"observed_collision_preserves_prior_receipt",applied,command,
      {authority with commandIdentityUnique := false},2⟩
  , ⟨"cross_session_collision_preserves_prior_receipt",applied ++ [.otherSession 901],
      {command with digest := 21},{authority with commandIdentityUnique := false},2⟩
  ]

def run (c : Case) := apply c.before.queue.scope c.now c.authority c.command c.before
example : (cases.map fun c => (run c).2.map (·.outcome)) =
    [some .applied,some .rejected,some .rejected,some .applied,some .rejected,
     some .rejected,none,some .rejected,some .rejected,some .rejected,
     some .rejected,some .applied,some .applied,none,none,none,none] := by native_decide

structure ClientObservationCase where
  name : String
  receiptOutcome : Outcome
  projectionRefreshSucceeded : Bool
  deriving DecidableEq, Repr

def clientObservationCases : List ClientObservationCase :=
  [ ⟨"applied_receipt_projection_failure", .applied, false⟩
  , ⟨"rejected_receipt_projection_failure", .rejected, false⟩
  , ⟨"applied_receipt_projection_success", .applied, true⟩
  ]

def runClientObservation (c : ClientObservationCase) : ClientObservation :=
  let receipt : Receipt := ⟨0, 0, c.receiptOutcome, []⟩
  observeProjectionRefresh (receiveValidatedReceipt receipt) c.projectionRefreshSucceeded

example : (clientObservationCases.map fun c =>
    (runClientObservation c).commandOutcome) = [.applied, .rejected, .applied] := by
  native_decide
example : (clientObservationCases.map fun c =>
    (runClientObservation c).projectionFresh) = [false, false, true] := by
  native_decide
end SessionQueue.InputEditCases
