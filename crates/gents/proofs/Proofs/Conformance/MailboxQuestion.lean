import Proofs.Automation.Mailbox.Question
import Proofs.Conformance.Family

namespace Conformance.MailboxQuestionContracts
open Lean Mailbox.Question

deriving instance ToJson for Policy

private def baseline : Mailbox.Item :=
  { identity :=
      { itemKey := "question"
        requesterDid := "owner"
        nodeDid := "node"
        sourceKind := .session
        sourceId := "request"
        kind := .ask }
    status := .open
    resolvedDocId := "" }

def policies : List Policy :=
  [0, 1, 2, 3].flatMap fun version =>
    [false, true].flatMap fun optional =>
      [0, 1, 2, 4, 5].flatMap fun optionCount =>
        [false, true].map fun freeText => { version, optional, optionCount, freeText }

structure QuestionInput where
  version : Nat
  optional : Bool
  optionCount : Nat
  freeText : Bool
  skip : Bool
  selected : Bool
  text : Bool
  deriving ToJson

structure QuestionOutput where
  valid : Bool
  accepted : Bool
  dismissible : Bool
  deriving ToJson

@[conformance_family]
def questions : Family QuestionInput QuestionOutput where
  inputs := policies.flatMap fun p =>
    [false, true].flatMap fun skip =>
      [false, true].flatMap fun selected =>
        [false, true].map fun text =>
          { version := p.version, optional := p.optional, optionCount := p.optionCount
            freeText := p.freeText, skip, selected, text }
  model := fun i =>
    let p : Policy :=
      { version := i.version, optional := i.optional, optionCount := i.optionCount
        freeText := i.freeText }
    { valid := valid p
      accepted := accepts p i.skip i.selected i.text
      dismissible := (dismiss? p.version baseline "owner").isSome }

structure WaitInput where
  policy : Policy
  isOpen : Bool
  sameParent : Bool
  completed : Bool
  deriving ToJson

structure WaitOutput where
  waiting : Bool
  deriving ToJson

def waitInputs : List WaitInput :=
  policies.flatMap fun policy =>
    [false, true].flatMap fun isOpen =>
      [false, true].flatMap fun sameParent =>
        [false, true].map fun completed => { policy, isOpen, sameParent, completed }

@[conformance_family]
def waits : Family WaitInput WaitOutput where
  inputs := waitInputs
  model := fun i => { waiting := awaitingAnswer i.policy i.isOpen i.sameParent i.completed }

@[conformance_family]
def awaiting : Family WaitInput WaitOutput where
  inputs := waitInputs
  model := fun i => { waiting := awaitingAnswer i.policy i.isOpen i.sameParent i.completed }

structure PresentationInput where
  policy : Policy
  multiSelect : Bool
  hasPrompt : Bool
  nonemptyPrompt : Bool
  order : Option (List Nat)
  deriving ToJson

structure PresentationOutput where
  valid : Bool
  deriving ToJson

private def presentation (policy : Policy) (multiSelect requestedPrompt nonemptyPrompt : Bool)
    (order : Option (List Nat)) : PresentationInput :=
  { policy, multiSelect, hasPrompt := requestedPrompt && policy.optionCount > 0, nonemptyPrompt
    order }

@[conformance_family]
def presentations : Family PresentationInput PresentationOutput where
  inputs := policies.flatMap fun policy =>
    let ranks := List.range policy.optionCount
    [ presentation policy false false true none
    , presentation policy false true true none
    , presentation policy false true false none
    , presentation policy true true true none
    , presentation policy true false true (some ranks)
    , presentation policy true false true (some ranks.reverse)
    , presentation policy true false true (some (0 :: ranks))
    , presentation policy true false true (some [policy.optionCount])
    , presentation policy true false true (some (ranks.drop 1))
    , presentation policy false false true (some ranks) ]
  model := fun i =>
    { valid := presentationValid i.policy i.multiSelect i.hasPrompt i.nonemptyPrompt i.order }

structure ReceiptInput where
  policy : Policy
  version : Option Nat
  deriving ToJson

structure ReceiptOutput where
  current : Bool
  deriving ToJson

@[conformance_family]
def receiptVersions : Family ReceiptInput ReceiptOutput where
  inputs := policies.flatMap fun policy =>
    [none, some 1, some 2, some 3, some 4].map fun version => { policy, version }
  model := fun i => { current := valid i.policy && receiptVersionCurrent i.policy.version i.version }

structure RuntimeInput where
  version : Option Nat
  replyVersion : Option Nat
  deriving ToJson

structure RuntimeOutput where
  supported : Bool
  deriving ToJson

@[conformance_family]
def runtimes : Family RuntimeInput RuntimeOutput where
  inputs := [none, some 3].flatMap fun replyVersion =>
    [none, some 1, some 2, some 3, some 4].map fun version => { version, replyVersion }
  model := fun i => { supported := runtimeSupports i.replyVersion i.version }

end Conformance.MailboxQuestionContracts
