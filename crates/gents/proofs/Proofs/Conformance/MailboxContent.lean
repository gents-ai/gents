import Proofs.Automation.Mailbox.Notification
import Proofs.Conformance.Family

namespace Conformance.MailboxContent

open Lean Mailbox.Notification

structure Step where
  requestId : String
  requester : String
  causeDocId : String
  title : String
  deriving ToJson

structure Trace where
  steps : List Step
  deriving ToJson

structure Observation where
  outcome : String
  title : String
  requesterAdmitted : Bool
  requestId : Option String
  causeDocId : Option String
  sameDocument : Bool
  deriving ToJson

structure Output where
  observations : List Observation
  deriving ToJson

def outcomeName : WriteOutcome → String
  | .created => "created"
  | .reused => "reused"
  | .updated => "updated"

def toStep (step : ContentStep) : Step :=
  { requestId := step.requestId, requester := step.requester,
    causeDocId := step.causeDocId, title := step.title }

def toContentStep (step : Step) : ContentStep :=
  ⟨step.requestId, step.requester, step.causeDocId, step.title⟩

def toObservation (o : ContentObservation) : Observation :=
  { outcome := outcomeName o.outcome, title := o.title,
    requesterAdmitted := o.requesterAdmitted, requestId := o.requestId,
    causeDocId := o.causeDocId, sameDocument := o.sameDocument }

@[conformance_family]
def provenance : Family Trace Output where
  inputs := [updatedSequence, repeatedSequence].map fun sequence => { steps := sequence.map toStep }
  model := fun t =>
    { observations := (observeContent none (t.steps.map toContentStep)).map toObservation }

end Conformance.MailboxContent
