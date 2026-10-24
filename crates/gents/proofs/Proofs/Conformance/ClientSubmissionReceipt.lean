import Proofs.ClientShell.SubmissionReceipt
import Lean

namespace Conformance.ClientSubmissionReceiptContracts
open Lean ClientSubmissionReceipt

inductive Action where
  | prepare (intent : Intent) (proposal : Prepared) (durable : Bool)
  | observe (caller : Caller) (prepared : Prepared) (outcome : Outcome)
  | acknowledge (caller : Caller) (prepared : Prepared)
  | reconnect

structure Case where
  name : String
  initial : Journal := []
  actions : List Action

def callerJson (caller : Caller) : Json := Json.mkObj
  [("requester", toJson caller.requester), ("token", toJson caller.token)]
def intentJson (intent : Intent) : Json := Json.mkObj
  [("caller", callerJson intent.caller), ("node", toJson intent.node),
   ("envelope", toJson intent.envelope)]
def preparedJson (prepared : Prepared) : Json := Json.mkObj
  [("session", toJson prepared.session), ("request", toJson prepared.request),
   ("signedBytes", toJson prepared.signedBytes)]
def outcomeJson (outcome : Outcome) : Json := toJson <| match outcome with
  | .unresolved => "unresolved"
  | .accepted => "accepted"
  | .notDelivered => "notDelivered"
def recordJson (record : Record) : Json := Json.mkObj
  [("intent", intentJson record.intent), ("prepared", preparedJson record.prepared),
   ("outcome", outcomeJson record.outcome), ("retainedBytes", toJson record.retainedBytes),
   ("acknowledged", toJson record.acknowledged)]
def journalJson (journal : Journal) : Json := toJson (journal.map recordJson)
def resultJson : Result → Json
  | .refused => Json.mkObj [("kind", toJson "refused")]
  | .reconcile prepared => Json.mkObj
    [("kind", toJson "reconcile"), ("prepared", preparedJson prepared)]
  | .settled prepared outcome => Json.mkObj
    [("kind", toJson "settled"), ("prepared", preparedJson prepared),
     ("outcome", outcomeJson outcome)]
def actionJson : Action → Json
  | .prepare intent proposal durable => Json.mkObj
    [("kind", toJson "prepare"), ("intent", intentJson intent),
     ("proposal", preparedJson proposal), ("durable", toJson durable)]
  | .observe caller prepared outcome => Json.mkObj
    [("kind", toJson "observe"), ("caller", callerJson caller),
     ("prepared", preparedJson prepared), ("outcome", outcomeJson outcome)]
  | .acknowledge caller prepared => Json.mkObj
    [("kind", toJson "acknowledge"), ("caller", callerJson caller),
     ("prepared", preparedJson prepared)]
  | .reconnect => Json.mkObj [("kind", toJson "reconnect")]

def run (journal : Journal) (action : Action) : Journal × Option Result :=
  match action with
  | .prepare intent proposal durable =>
    let (next, output) := prepare journal intent proposal durable
    (next, some output)
  | .observe caller prepared outcome => (observe journal caller prepared outcome, none)
  | .acknowledge caller prepared => (acknowledge journal caller prepared, none)
  | .reconnect => (journal, none)

def traces (journal : Journal) : List Action → List Json
  | [] => []
  | action :: actions =>
    let (next, output) := run journal action
    Json.mkObj [("input", actionJson action), ("before", journalJson journal),
      ("after", journalJson next), ("output", output.map resultJson |>.getD Json.null)] ::
      traces next actions

def cases : List Case :=
  let caller : Caller := ⟨1, 10⟩
  let intent : Intent := ⟨caller, 2, 100⟩
  let prepared : Prepared := ⟨20, 30, 300⟩
  let fresh : Prepared := ⟨21, 31, 301⟩
  let start := Action.prepare intent prepared true
  let retry := Action.prepare intent fresh true
  let accepted := Action.observe caller prepared .accepted
  let ack := Action.acknowledge caller prepared
  [ ⟨"lost_preparation_reply", [], [start, .reconnect, retry]⟩
  , ⟨"lost_accepted_reply", [], [start, accepted, .reconnect, retry]⟩
  , ⟨"acknowledgement_loss", [], [start, accepted, ack, .reconnect, ack, retry]⟩
  , ⟨"stale_form_after_acknowledgement", [], [start, accepted, ack, retry]⟩
  , ⟨"confirmed_non_delivery_retains_identity", [],
      [start, .observe caller prepared .notDelivered, ack, .reconnect, retry]⟩
  , ⟨"unresolved_cannot_be_acknowledged", [], [start, ack, .reconnect, retry]⟩
  , ⟨"distinct_authored_operations", [], [start,
      .prepare { intent with caller := ⟨1, 11⟩ } fresh true]⟩
  , ⟨"different_envelope_refused", [], [start,
      .prepare { intent with envelope := 101 } fresh true]⟩
  , ⟨"different_runtime_refused", [], [start,
      .prepare { intent with node := 3 } fresh true]⟩
  , ⟨"different_signer_is_distinct", [], [start,
      .prepare { intent with caller := ⟨2, 10⟩ } fresh true]⟩
  , ⟨"failed_durable_prepare_has_no_effect", [],
      [.prepare intent prepared false, .reconnect, retry]⟩
  , ⟨"same_preparation_does_not_depend_on_new_write", [],
      [start, .reconnect, .prepare intent fresh false]⟩
  , ⟨"foreign_receipt_refused", [],
      [start, .observe caller { prepared with signedBytes := 999 } .accepted, retry]⟩
  , ⟨"foreign_acknowledgement_refused", [],
      [start, accepted, .acknowledge caller fresh, retry]⟩
  , ⟨"confirmed_outcome_is_monotonic", [],
      [start, accepted, .observe caller prepared .notDelivered, retry]⟩
  , ⟨"independent_caller_cannot_reuse_request", [],
      [start, .prepare { intent with caller := ⟨1, 11⟩ } prepared true]⟩
  , ⟨"ambiguous_caller_records_refused",
      [⟨intent, prepared, .unresolved, true, false⟩, ⟨intent, fresh, .unresolved, true, false⟩],
      [.observe caller prepared .accepted, ack, retry]⟩
  ]

def casesJson : String := (toJson (cases.map fun c => Json.mkObj
  [("name", toJson c.name), ("initial", journalJson c.initial),
   ("trace", toJson (traces c.initial c.actions))])).compress

example : cases.length = 17 := by decide
end Conformance.ClientSubmissionReceiptContracts

def main : IO Unit := IO.println Conformance.ClientSubmissionReceiptContracts.casesJson
