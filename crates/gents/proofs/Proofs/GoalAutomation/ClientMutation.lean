import Proofs.GoalAutomation.ClientControl
import Proofs.Session.CommandAuthority

namespace GoalAutomation.ClientMutation

structure Command where
  id : Nat
  digest : Nat
  hasPeer : Bool
  sessionDocument : String
  issuedAt : Nat
  expiresAt : Nat
  expected : ClientControl.Revision
  operation : ClientControl.Operation
  deriving DecidableEq, Repr

inductive Outcome where
  | applied | rejected
  deriving DecidableEq, Repr

structure Receipt where
  commandId : Nat
  digest : Nat
  outcome : Outcome
  deriving DecidableEq, Repr

/-- The agent that processes a command signs its receipt. A Node signature or
another agent's signature is not evidence of this selected agent's work. -/
def receiptSignerAccepted (selected signer : Nat) (signatureValid : Bool) : Bool :=
  selected == signer && signatureValid

theorem foreign_receipt_signer_is_rejected (selected signer : Nat)
    (different : selected ≠ signer) : receiptSignerAccepted selected signer true = false := by
  simp [receiptSignerAccepted, different]

/-- The selected agent creates the receipt under its own native ACP ownership.
The requester receives an explicit reader relationship in that same transaction,
so a later session or invoke revocation does not hide the result of its own
command. The command author cannot create a competing receipt for the command,
or alter or delete the agent's receipt; native creation binds the authenticated
creator to the selected Agent, and the receipt policy reserves mutation to its
owner. The full signed digest binds the immutable receipt to its command. -/
structure OwnedReceipt where
  agent : Nat
  author : Nat
  commandId : Nat
  digest : Nat
  deriving DecidableEq, Repr

def receiptCanCreate (selected actor : Nat) : Bool := selected == actor
def receiptCanRead (receipt : OwnedReceipt) (actor : Nat) : Bool :=
  actor == receipt.agent || actor == receipt.author
def receiptCanMutate (receipt : OwnedReceipt) (actor : Nat) : Bool :=
  actor == receipt.agent
def receiptMatches (receipt : OwnedReceipt) (commandId digest : Nat) : Bool :=
  receipt.commandId == commandId && receipt.digest == digest

theorem requester_cannot_erase_receipt (receipt : OwnedReceipt)
    (different : receipt.author ≠ receipt.agent) :
    receiptCanMutate receipt receipt.author = false := by
  simp [receiptCanMutate, different]

theorem revoked_requester_reads_own_receipt (receipt : OwnedReceipt) :
    receiptCanRead receipt receipt.author = true := by
  simp [receiptCanRead]

theorem receipt_binds_command (receipt : OwnedReceipt) :
    receiptMatches receipt receipt.commandId receipt.digest = true := by
  simp [receiptMatches]

structure State where
  goal : ClientControl.Snapshot
  /-- Only receipts whose selected-agent signature and exact command digest verified. -/
  receipts : List Receipt
  deriving DecidableEq, Repr

structure Facts where
  authority : AgentSession.CommandAuthority.Facts
  commandIdentityUnique : Bool
  unfinishedRequest : Bool
  resume : Option OperatorResume.Request
  sessionDocument : String
  deriving DecidableEq, Repr

/-- The native reader first authenticates the retained author signature and
binds the full signed command to its digest. Observation then reads only that
original command identity and digest. Current
Goal revisions, authorization changes and time cannot turn a missing receipt
into confirmed application or authorize publication of a replacement intent. -/
def recoverReceipt (s : State) (command : Command) (identityUnique : Bool) : Option Receipt :=
  if !identityUnique then none
  else (s.receipts.find? fun receipt => receipt.commandId == command.id).filter
    (fun receipt => receipt.digest == command.digest)

theorem recovery_refuses_ambiguous_identity (s : State) (command : Command) :
    recoverReceipt s command false = none := by
  simp [recoverReceipt]

theorem recovery_without_receipt_is_unconfirmed (s : State) (command : Command)
    (h : s.receipts.find? (fun receipt => receipt.commandId == command.id) = none) :
    recoverReceipt s command true = none := by
  simp [recoverReceipt, h]

theorem recovery_returns_exact_recorded_result (s : State) (command : Command)
    (receipt : Receipt)
    (h : s.receipts.find? (fun receipt => receipt.commandId == command.id) = some receipt)
    (digest : receipt.digest = command.digest) :
    recoverReceipt s command true = some receipt := by
  simp [recoverReceipt, h, digest]

def authorized (command : Command) (facts : Facts) (now : Nat) : Bool :=
  AgentSession.CommandAuthority.authorized facts.authority command.hasPeer now command.expiresAt &&
    facts.sessionDocument == command.sessionDocument

def decideFresh (s : State) (command : Command) (facts : Facts) (now : Nat)
    (nextComposite : String) : State × Option Receipt :=
  if facts.authority.signatureValid && now < command.issuedAt then (s, none)
  else
    let authority := authorized command facts now
    let post := ClientControl.apply? s.goal command.expected nextComposite command.operation
      ⟨authority, facts.unfinishedRequest, facts.resume⟩
    let receipt := Receipt.mk command.id command.digest (if post.isSome then .applied else .rejected)
    (⟨post.getD s.goal, s.receipts ++ [receipt]⟩, some receipt)

/-- A command's effect and signed receipt commit together. Transport loss does
not replay the mutation. Erasing a receipt cannot restore an old document CID,
including after a pause/resume status round trip. -/
def apply (s : State) (command : Command) (facts : Facts) (now : Nat)
    (nextComposite : String) (commit : Bool) : State × Option Receipt :=
  if !facts.commandIdentityUnique then (s, none)
  else match s.receipts.find? (fun receipt => receipt.commandId == command.id) with
  | some receipt => if receipt.digest = command.digest then (s, some receipt) else (s, none)
  | none => if commit then decideFresh s command facts now nextComposite else (s, none)

theorem discarded_command_preserves_goal (s : State) (command : Command) (facts : Facts)
    (now : Nat) (next : String) : (apply s command facts now next false).1 = s := by
  unfold apply
  split
  · rfl
  · split
    · split <;> rfl
    · rfl

theorem identity_collision_cannot_change_goal (s : State) (command : Command) (facts : Facts)
    (now : Nat) (next : String) (commit : Bool) (h : facts.commandIdentityUnique = false) :
    apply s command facts now next commit = (s, none) := by
  simp [apply, h]

theorem recorded_replay_preserves_goal (s : State) (command : Command) (facts : Facts)
    (now : Nat) (next : String) (commit : Bool) (receipt : Receipt)
    (h : s.receipts.find? (fun receipt => receipt.commandId == command.id) = some receipt) :
    (apply s command facts now next commit).1 = s := by
  by_cases unique : facts.commandIdentityUnique <;>
    by_cases digest : receipt.digest = command.digest <;> simp [apply, unique, h, digest]

theorem stale_revision_preserves_goal (s : State) (command : Command) (facts : Facts)
    (now : Nat) (next : String) (h : s.goal.revision ≠ command.expected) :
    (decideFresh s command facts now next).1.goal = s.goal := by
  simp [decideFresh, ClientControl.stale_revision_cannot_change_goal s.goal command.expected
    next command.operation ⟨authorized command facts now, facts.unfinishedRequest, facts.resume⟩ h]
  split <;> rfl

theorem recreated_session_preserves_goal (s : State) (command : Command) (facts : Facts)
    (now : Nat) (next : String) (h : facts.sessionDocument ≠ command.sessionDocument) :
    (decideFresh s command facts now next).1.goal = s.goal := by
  simp [decideFresh, authorized, h, ClientControl.apply?]
  split <;> rfl

end GoalAutomation.ClientMutation
