import Proofs.Session.InputEdit
import Proofs.Session.ManagementCases

namespace SessionQueue.InputEditCases
open InputEdit ManagementCases FoldCases

def authority : Authority := ⟨true,true,true,false,false,true,true,true⟩
def command : Command := ⟨10,20,some 1,true,1,10,[2],0,1,[replacement 3 2]⟩
def state : State := ⟨initial [pending 2],[]⟩
structure Case where
  name : String
  before : State
  command : Command
  authority : Authority
  now : Nat

def cases : List Case :=
  [ ⟨"remote_applied",state,command,authority,2⟩
  , ⟨"expired_rejected",state,command,authority,10⟩
  , ⟨"future_issuance_rejected",state,command,authority,0⟩
  , ⟨"signature_rejected",state,command,{authority with signatureValid := false},2⟩
  , ⟨"replacement_signature_rejected",state,command,{authority with replacementsValid := false},2⟩
  , ⟨"revoked_enrollment_rejected",state,command,{authority with enrollmentFresh := false},2⟩
  , ⟨"missing_route_rejected",state,command,{authority with routeApplied := false},2⟩
  , ⟨"foreign_session_rejected",state,command,{authority with sessionOwned := false},2⟩
  , ⟨"local_self_applied",state,{command with hasPeer := false},
      {authority with localSelf := true, requesterIsNode := true},2⟩
  , ⟨"local_foreign_rejected",state,{command with hasPeer := false},
      {authority with localSelf := true},2⟩
  , ⟨"stale_queue_rejected",state,{command with expected := []},authority,2⟩
  , ⟨"exact_replay_after_expiry",(apply queue.scope 2 authority command state).1,command,authority,100⟩
  , ⟨"command_identity_collision",(apply queue.scope 2 authority command state).1,
      {command with digest := 21},authority,2⟩
  , ⟨"simultaneous_command_identity_collision",state,command,
      {authority with commandIdentityUnique := false},2⟩
  , ⟨"same_intent_physical_collision",state,command,
      {authority with commandIdentityUnique := false},2⟩
  , ⟨"observed_collision_preserves_prior_receipt",
      (apply queue.scope 2 authority command state).1,command,
      {authority with commandIdentityUnique := false},2⟩
  , ⟨"cross_session_collision_preserves_prior_receipt",
      {state with
        queue := {state.queue with scope := {state.queue.scope with session := 901}}
        receipts := (apply queue.scope 2 authority command state).1.receipts},
      {command with digest := 21}, {authority with commandIdentityUnique := false},2⟩
  ]

def run (c : Case) := apply c.before.queue.scope c.now c.authority c.command c.before
example : (cases.map fun c => (run c).2.map (·.outcome)) =
    [some .applied,some .rejected,some .rejected,some .rejected,some .rejected,
     some .rejected,some .rejected,some .rejected,some .applied,some .rejected,
     some .rejected,some .applied,none,none,none,none,none] := by native_decide
end SessionQueue.InputEditCases
