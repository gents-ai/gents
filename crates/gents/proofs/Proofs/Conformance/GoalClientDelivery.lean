import Proofs.GoalAutomation.ClientDelivery
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

def intentJson (intent : Intent) : Json := Json.mkObj
  [("client", toJson intent.client), ("node", toJson intent.node),
   ("command", toJson intent.command), ("session", toJson intent.session),
   ("authored", toJson intent.authored)]

def preparedJson (prepared : Prepared) : Json := Json.mkObj
  [("command", toJson prepared.command), ("digest", toJson prepared.digest),
   ("initialRequest", toJson prepared.initialRequest)]

def receiptJson (receipt : GoalAutomation.ClientMutation.Receipt) : Json := Json.mkObj
  [("command", toJson receipt.commandId), ("digest", toJson receipt.digest),
   ("outcome", toJson <| if receipt.outcome = .applied then "applied" else "rejected")]

def recordJson (record : Record) : Json := Json.mkObj
  [("intent", intentJson record.intent), ("prepared", preparedJson record.prepared),
   ("receipt", record.receipt.map receiptJson |>.getD Json.null),
   ("retainedBytes", toJson record.retainedBytes), ("acknowledged", toJson record.acknowledged)]

def journalJson (journal : Journal) : Json := toJson (journal.map recordJson)

def resultJson : Result → Json
  | .refused => Json.mkObj [("kind", toJson "refused")]
  | .unconfirmed => Json.mkObj [("kind", toJson "unconfirmed")]
  | .pending prepared => Json.mkObj
      [("kind", toJson "pending"), ("prepared", preparedJson prepared)]
  | .confirmed prepared receipt => Json.mkObj
      [("kind", toJson "confirmed"), ("prepared", preparedJson prepared),
       ("receipt", receiptJson receipt)]

def actionJson : Action → Json
  | .reserve intent prepared durable => Json.mkObj
      [("kind", toJson "reserve"), ("intent", intentJson intent),
       ("prepared", preparedJson prepared), ("durable", toJson durable)]
  | .query intent => Json.mkObj [("kind", toJson "query"), ("intent", intentJson intent)]
  | .resume intent => Json.mkObj [("kind", toJson "resume"), ("intent", intentJson intent)]
  | .observe intent receipt verified => Json.mkObj
      [("kind", toJson "observe"), ("intent", intentJson intent),
       ("receipt", receiptJson receipt), ("verified", toJson verified)]
  | .acknowledge intent prepared => Json.mkObj
      [("kind", toJson "acknowledge"), ("intent", intentJson intent),
       ("prepared", preparedJson prepared)]
  | .reconnect => Json.mkObj [("kind", toJson "reconnect")]

def output (result : Result) (effect : Option Prepared := none) : Json := Json.mkObj
  [("result", resultJson result), ("publish", effect.map preparedJson |>.getD Json.null)]

def run (journal : Journal) : Action → Journal × Json
  | .reserve intent prepared durable =>
      let (next, result) := reserve journal intent prepared durable
      (next, output result)
  | .query intent => (journal, output (query journal intent))
  | .resume intent =>
      let (result, effect) := resume journal intent
      (journal, output result effect)
  | .observe intent receipt verified => (observe journal intent receipt verified, Json.null)
  | .acknowledge intent prepared => (acknowledge journal intent prepared, Json.null)
  | .reconnect => (journal, Json.null)

def traces (journal : Journal) : List Action → List Json
  | [] => []
  | action :: rest =>
      let (next, result) := run journal action
      Json.mkObj [("input", actionJson action), ("before", journalJson journal),
        ("after", journalJson next), ("output", result)] :: traces next rest

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

def casesJson : Json := toJson <| cases.map fun c => Json.mkObj
  [("name", toJson c.name), ("initial", journalJson c.initial),
   ("trace", toJson (traces c.initial c.actions))]

example : cases.length = 21 := by native_decide

end Conformance.GoalClientDeliveryContracts

def main : IO Unit := IO.println Conformance.GoalClientDeliveryContracts.casesJson
