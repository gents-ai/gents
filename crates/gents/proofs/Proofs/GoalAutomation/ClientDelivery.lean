import Proofs.GoalAutomation.ClientMutation

namespace GoalAutomation.ClientDelivery

/-- The native owner authenticates `client` independently of the runtime
requester. `authored` binds the full action, observed revision, Agent, captured
input and selected destination; equality is never inferred from display text. -/
structure Intent where
  client : Nat
  node : Nat
  command : Nat
  session : Nat
  authored : Nat
  deriving DecidableEq, Repr

/-- `digest` denotes the existing Goal owner's exact signed command, including
its timestamps and signed first request. Native decoding and signature checking
establish that binding before these decisions; recovery cannot re-sign it. -/
structure Prepared where
  command : Nat
  digest : Nat
  initialRequest : Option Nat
  deriving DecidableEq, Repr

structure Record where
  intent : Intent
  prepared : Prepared
  receipt : Option ClientMutation.Receipt := none
  retainedBytes : Bool := true
  acknowledged : Bool := false
  deriving DecidableEq, Repr

abbrev Journal := List Record

inductive Result where
  | refused
  | unconfirmed
  | pending (prepared : Prepared)
  | confirmed (prepared : Prepared) (receipt : ClientMutation.Receipt)
  deriving DecidableEq, Repr

def receiptBound (prepared : Prepared) (receipt : ClientMutation.Receipt) : Bool :=
  receipt.commandId == prepared.command && receipt.digest == prepared.digest

def valid (record : Record) : Bool :=
  record.intent.command == record.prepared.command &&
    record.retainedBytes != record.acknowledged &&
    (!record.acknowledged || record.receipt.isSome) &&
    record.receipt.all (receiptBound record.prepared)

/-- The existing journal reserves one physical command identity. A record in a
foreign client, runtime or destination scope is a refusal, not missing input. -/
def query (journal : Journal) (intent : Intent) : Result :=
  let records := journal.filter fun record => record.intent.command == intent.command
  match records with
  | [] => .unconfirmed
  | [record] =>
      if record.intent != intent || !valid record then .refused
      else match record.receipt with
      | some receipt => .confirmed record.prepared receipt
      | none => if record.retainedBytes then .pending record.prepared else .refused
  | _ => .refused

/-- Durable reservation is atomic in the existing encrypted journal. A racing
proposal cannot replace the winner's signature or initial request. A failed or
ambiguous local write never authorizes publication. -/
def reserve (journal : Journal) (intent : Intent) (prepared : Prepared)
    (durable : Bool) : Journal × Result :=
  match query journal intent with
  | .unconfirmed =>
      let record : Record := ⟨intent, prepared, none, true, false⟩
      if durable && valid record then (journal ++ [record], .pending prepared)
      else (journal, .refused)
  | result => (journal, result)

/-- Observation is a query only. Publication is a separate explicit operation,
and its only possible effect is the already retained signed preparation. -/
def resume (journal : Journal) (intent : Intent) : Result × Option Prepared :=
  let result := query journal intent
  (result, match result with | .pending prepared => some prepared | _ => none)

/-- `verified` comes from the existing runtime receipt signature/scope owner.
Both applied and rejected receipts settle the command. A later observation
cannot replace a confirmed result or authorize publication after acknowledgement. -/
def observe (journal : Journal) (intent : Intent) (receipt : ClientMutation.Receipt)
    (verified : Bool) : Journal :=
  if !verified then journal
  else match query journal intent with
  | .pending prepared =>
      if receiptBound prepared receipt then
        journal.map fun record =>
          if record.intent = intent then { record with receipt := some receipt } else record
      else journal
  | _ => journal

/-- The caller acknowledges only after saving its confirmed workflow. Signed
bytes may then retire while the exact binding and result remain recoverable. -/
def acknowledge (journal : Journal) (intent : Intent) (prepared : Prepared) : Journal :=
  match query journal intent with
  | .confirmed original _ =>
      if original = prepared then
        journal.map fun record =>
          if record.intent = intent then
            { record with retainedBytes := false, acknowledged := true }
          else record
      else journal
  | _ => journal

theorem missing_preparation_cannot_publish (intent : Intent) :
    (resume [] intent).2 = none := by
  simp [resume, query]

theorem failed_reservation_cannot_publish (journal : Journal) (intent : Intent)
    (prepared : Prepared) (missing : query journal intent = .unconfirmed) :
    reserve journal intent prepared false = (journal, .refused) := by
  simp [reserve, missing]

theorem unverified_receipt_cannot_settle (journal : Journal) (intent : Intent)
    (receipt : ClientMutation.Receipt) : observe journal intent receipt false = journal := by
  simp [observe]

theorem pending_cannot_be_acknowledged (journal : Journal) (intent : Intent)
    (original prepared : Prepared) (pending : query journal intent = .pending original) :
    acknowledge journal intent prepared = journal := by
  simp [acknowledge, pending]

theorem confirmed_cannot_publish (journal : Journal) (intent : Intent)
    (prepared : Prepared) (receipt : ClientMutation.Receipt)
    (confirmed : query journal intent = .confirmed prepared receipt) :
    (resume journal intent).2 = none := by
  simp [resume, confirmed]

theorem racing_proposal_keeps_preparation (journal : Journal) (intent : Intent)
    (original proposed : Prepared) (durable : Bool)
    (pending : query journal intent = .pending original) :
    reserve journal intent proposed durable = (journal, .pending original) := by
  simp [reserve, pending]

end GoalAutomation.ClientDelivery
