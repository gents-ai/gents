import Proofs.Automation.GoalAutomation.ClientDelivery
import Proofs.Conformance.Family
import Lean

namespace Conformance.GoalClientDeliveryContracts
open Lean GoalAutomation.ClientDelivery

inductive Action where
  | reserve (intent : Intent) (prepared : Prepared) (durable : Bool)
  | query (intent : Intent)
  | resume (intent : Intent)
  | observe (intent : Intent) (receipt : GoalAutomation.ClientMutation.Receipt) (verified : Bool)
  | acknowledge (intent : Intent) (prepared : Prepared)
  | reconnect

structure Case where
  name : String
  initial : Journal := []
  actions : List Action

structure IntentWire where
  client : Nat
  node : Nat
  command : Nat
  session : Nat
  authored : Nat
  deriving ToJson

structure PreparedWire where
  command : Nat
  digest : Nat
  initialRequest : Option Nat
  deriving ToJson, BEq

structure ReceiptWire where
  command : Nat
  digest : Nat
  outcome : String
  deriving ToJson

structure RecordWire where
  intent : IntentWire
  prepared : PreparedWire
  receipt : Option ReceiptWire
  retainedBytes : Bool
  acknowledged : Bool
  deriving ToJson

structure ResultWire where
  kind : String
  prepared : Option PreparedWire
  receipt : Option ReceiptWire
  deriving ToJson

structure OutputWire where
  result : ResultWire
  publish : Option PreparedWire
  deriving ToJson

structure ActionWire where
  kind : String
  intent : Option IntentWire
  prepared : Option PreparedWire
  durable : Option Bool
  receipt : Option ReceiptWire
  verified : Option Bool
  deriving ToJson

structure StepWire where
  before : List RecordWire
  after : List RecordWire
  output : Option OutputWire
  deriving ToJson

structure TraceInput where
  name : String
  initial : List RecordWire
  actions : List ActionWire
  deriving ToJson

def intentWire (intent : Intent) : IntentWire :=
  { client := intent.client, node := intent.node, command := intent.command
    session := intent.session, authored := intent.authored }

def preparedWire (prepared : Prepared) : PreparedWire :=
  { command := prepared.command, digest := prepared.digest
    initialRequest := prepared.initialRequest }

def receiptWire (receipt : GoalAutomation.ClientMutation.Receipt) : ReceiptWire :=
  { command := receipt.commandId, digest := receipt.digest
    outcome := if receipt.outcome = .applied then "applied" else "rejected" }

def recordWire (record : Record) : RecordWire :=
  { intent := intentWire record.intent, prepared := preparedWire record.prepared
    receipt := record.receipt.map receiptWire, retainedBytes := record.retainedBytes
    acknowledged := record.acknowledged }

def resultWire : Result → ResultWire
  | .refused => { kind := "refused", prepared := none, receipt := none }
  | .unconfirmed => { kind := "unconfirmed", prepared := none, receipt := none }
  | .pending prepared => { kind := "pending", prepared := some (preparedWire prepared), receipt := none }
  | .confirmed prepared receipt =>
      { kind := "confirmed", prepared := some (preparedWire prepared)
        receipt := some (receiptWire receipt) }

def actionWire : Action → ActionWire
  | .reserve intent prepared durable =>
      { kind := "reserve", intent := some (intentWire intent), prepared := some (preparedWire prepared)
        durable := some durable, receipt := none, verified := none }
  | .query intent =>
      { kind := "query", intent := some (intentWire intent), prepared := none, durable := none
        receipt := none, verified := none }
  | .resume intent =>
      { kind := "resume", intent := some (intentWire intent), prepared := none, durable := none
        receipt := none, verified := none }
  | .observe intent receipt verified =>
      { kind := "observe", intent := some (intentWire intent), prepared := none, durable := none
        receipt := some (receiptWire receipt), verified := some verified }
  | .acknowledge intent prepared =>
      { kind := "acknowledge", intent := some (intentWire intent)
        prepared := some (preparedWire prepared), durable := none, receipt := none, verified := none }
  | .reconnect =>
      { kind := "reconnect", intent := none, prepared := none, durable := none, receipt := none
        verified := none }

def output (result : Result) (effect : Option Prepared := none) : OutputWire :=
  { result := resultWire result, publish := effect.map preparedWire }

def run (journal : Journal) : Action → Journal × Option OutputWire
  | .reserve intent prepared durable =>
      let (next, result) := reserve journal intent prepared durable
      (next, some (output result))
  | .query intent => (journal, some (output (query journal intent)))
  | .resume intent =>
      let (result, effect) := resume journal intent
      (journal, some (output result effect))
  | .observe intent receipt verified => (observe journal intent receipt verified, none)
  | .acknowledge intent prepared => (acknowledge journal intent prepared, none)
  | .reconnect => (journal, none)

def traces (journal : Journal) : List Action → List StepWire
  | [] => []
  | action :: rest =>
      let (next, result) := run journal action
      { before := journal.map recordWire, after := next.map recordWire, output := result }
        :: traces next rest

def cases : List Case :=
  let intent : Intent := ⟨1, 2, 10, 20, 30⟩
  let prepared : Prepared := ⟨10, 40, some 50⟩
  let fresh : Prepared := ⟨10, 41, some 51⟩
  let applied : GoalAutomation.ClientMutation.Receipt := ⟨10, 40, .applied⟩
  let rejected : GoalAutomation.ClientMutation.Receipt := ⟨10, 40, .rejected⟩
  let record : Record := ⟨intent, prepared, none, true, false⟩
  let start := Action.reserve intent prepared true
  let check := Action.query intent
  let send := Action.resume intent
  let settle := Action.observe intent applied true
  let ack := Action.acknowledge intent prepared
  [ ⟨"missing_and_failed_preparation", [],
      [check, send, .reserve intent prepared false, check, send, start]⟩
  , ⟨"lost_preparation_reply", [], [start, .reconnect, check, send]⟩
  , ⟨"lost_applied_reply", [], [start, settle, .reconnect, check, send]⟩
  , ⟨"lost_rejected_reply", [],
      [start, .observe intent rejected true, .reconnect, check, send]⟩
  , ⟨"acknowledgement_loss", [], [start, settle, ack, .reconnect, ack, check, send]⟩
  , ⟨"pending_acknowledgement_is_refused", [], [start, ack, check, send]⟩
  , ⟨"changed_authored_input", [],
      [start, .reserve { intent with authored := 31 } fresh true]⟩
  , ⟨"foreign_native_client", [], [start, .query { intent with client := 3 }]⟩
  , ⟨"foreign_runtime", [], [start, .resume { intent with node := 3 }]⟩
  , ⟨"foreign_destination", [], [start, .resume { intent with session := 21 }]⟩
  , ⟨"racing_first_request_proposal", [], [start, .reserve intent fresh true, send]⟩
  , ⟨"unverified_receipt", [], [start, .observe intent applied false, check, send]⟩
  , ⟨"foreign_receipt_digest", [],
      [start, .observe intent { applied with digest := 41 } true, check]⟩
  , ⟨"foreign_receipt_command", [],
      [start, .observe intent { applied with commandId := 11 } true, check]⟩
  , ⟨"confirmed_receipt_cannot_change", [],
      [start, settle, .observe intent rejected true, check, send]⟩
  , ⟨"duplicate_physical_reservation", [record, record], [check, send, ack]⟩
  , ⟨"corrupt_early_acknowledgement", [{ record with acknowledged := true }], [check, send]⟩
  , ⟨"retired_bytes_without_acknowledgement", [{ record with retainedBytes := false }],
      [check, send]⟩
  , ⟨"proposal_command_mismatch", [], [.reserve intent { prepared with command := 11 } true]⟩
  , ⟨"stale_form_after_acknowledgement", [],
      [start, settle, ack, .reserve intent fresh true, check, send]⟩
  , let control := { intent with command := 11, authored := 31 }
    let signed : Prepared := ⟨11, 42, none⟩
    ⟨"control_without_first_request", [],
      [.reserve control signed true, .resume control,
       .observe control ⟨11, 42, .applied⟩ true, .query control,
       .acknowledge control signed, .reconnect, .query control, .resume control]⟩
  ]

def traceInput (c : Case) : TraceInput :=
  { name := c.name, initial := c.initial.map recordWire, actions := c.actions.map actionWire }

def modeledSteps (i : TraceInput) : List StepWire :=
  match cases.find? (·.name == i.name) with
  | some c => traces c.initial c.actions
  | none => []

/-- The wire input names its case and carries its actions; the model executes
the case's own journal and actions, whose intents and receipts it builds. -/
@[conformance_family]
def trace : Conformance.Family TraceInput (List StepWire) where
  inputs := cases.map traceInput
  model := modeledSteps

/-- The durable journal starts empty and every reservation it receives is
durable and every receipt verified: a persisted record cannot hold a failed
preparation, an unverified receipt or a pre-existing corrupt journal. -/
def durableCases : List Case :=
  cases.filter fun c => c.initial.isEmpty && c.actions.all fun
    | .reserve _ _ durable => durable
    | .observe _ _ verified => verified
    | _ => true

@[conformance_family]
def durableTrace : Conformance.Family TraceInput (List StepWire) where
  inputs := durableCases.map traceInput
  model := modeledSteps

/-- The reopened journal reserves, settles and acknowledges one command,
reopening its store between those steps: the journal after each of the
first three actions of the acknowledgement-loss history. -/
@[conformance_family]
def reopenedAcknowledgement : Conformance.Family TraceInput (List (List RecordWire)) where
  inputs := (cases.filter (·.name == "acknowledgement_loss")).map fun c =>
    traceInput { c with actions := c.actions.take 3 }
  model := fun i => ((modeledSteps i).take 3).map (·.after)

structure RacingOutput where
  sharedPreparation : Bool
  records : Nat
  deriving ToJson

/-- Two signed first-request proposals for one intent race to reserve: both
reservations decide the same preparation and the journal holds one record. -/
@[conformance_family]
def racingProposals : Conformance.Family TraceInput RacingOutput where
  inputs := (cases.filter (·.name == "racing_first_request_proposal")).map fun c =>
    traceInput { c with actions := c.actions.take 2 }
  model := fun i =>
    let steps := (modeledSteps i).take 2
    { sharedPreparation := match steps with
        | [first, second] =>
            (first.output.map (·.result.prepared)) == (second.output.map (·.result.prepared))
        | _ => false
      records := ((steps.getLast?).map (·.after.length)).getD 0 }

example : cases.length = 21 := by native_decide

end Conformance.GoalClientDeliveryContracts
