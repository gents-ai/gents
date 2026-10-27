import Proofs.Mailbox.Transition

namespace Mailbox.Question

/-- Version one remains readable. Version two explicitly distinguishes optional
questions and admits free-text-only prompts. Skip is an explicit reply, never a
default selection or a consequence of silence. -/
structure Policy where
  version : Nat
  optional : Bool
  optionCount : Nat
  freeText : Bool
  deriving DecidableEq, Repr

def modern (policy : Policy) : Bool := policy.version == 2 || policy.version == 3

def valid (policy : Policy) : Bool :=
  ((policy.version == 1 && !policy.optional) || modern policy) &&
    ((2 ≤ policy.optionCount && policy.optionCount ≤ 4) ||
      (modern policy && policy.optionCount == 0 && policy.freeText))

def accepts (policy : Policy) (skip selected text : Bool) : Bool :=
  valid policy &&
    if skip then modern policy && policy.optional && !selected && !text
    else (!text || policy.freeText) &&
      ((!selected || policy.optionCount > 0) && (selected || text))

/-- A required modern question holds only its completed asking request's
automatic continuation. Optional prompts, other requests and cancelled work do
not hold independent work. An accepted reply closes the item through its
existing signed owner; merely showing a question grants no access. -/
def awaitingAnswer (policy : Policy) (isOpen sameParent completed : Bool) : Bool :=
  valid policy && modern policy && !policy.optional &&
    isOpen && sameParent && completed

theorem optional_does_not_hold_work (policy : Policy) (isOpen sameParent completed : Bool)
    (h : policy.optional = true) : awaitingAnswer policy isOpen sameParent completed = false := by
  simp [awaitingAnswer, h]

theorem answered_does_not_hold_work (policy : Policy) (sameParent completed : Bool) :
    awaitingAnswer policy false sameParent completed = false := by
  simp [awaitingAnswer]

theorem another_request_does_not_hold_work (policy : Policy) (isOpen completed : Bool) :
    awaitingAnswer policy isOpen false completed = false := by
  simp [awaitingAnswer]

theorem unfinished_or_cancelled_does_not_hold_work (policy : Policy) (isOpen sameParent : Bool) :
    awaitingAnswer policy isOpen sameParent false = false := by
  simp [awaitingAnswer]

/-- Prepared prompts are editable decision text, never a worker or permission
receipt. They require a single choice and explicit free text support. -/
def promptAllowed (policy : Policy) (multiSelect hasPrompt nonempty : Bool) : Bool :=
  !hasPrompt || (policy.version == 3 && policy.freeText && !multiSelect && nonempty)

/-- Suggested ranks name every offered choice exactly once. They do not select
or submit an answer; the person must explicitly use or edit that order. -/
def orderAllowed (policy : Policy) (multiSelect : Bool) (order : Option (List Nat)) : Bool :=
  match order with
  | none => true
  | some ranks => policy.version == 3 && multiSelect && policy.optionCount > 0 &&
      ranks.length == policy.optionCount && ranks.all (· < policy.optionCount) &&
      decide ranks.Nodup

def presentationValid (policy : Policy) (multiSelect hasPrompt nonempty : Bool)
    (order : Option (List Nat)) : Bool :=
  valid policy && promptAllowed policy multiSelect hasPrompt nonempty &&
    orderAllowed policy multiSelect order

/-- Version-two reply bytes predate this optional discriminator. Prepared
version-three metadata must retain its version inside the signed reply. -/
def receiptVersionCurrent (questionVersion : Nat) (replyVersion : Option Nat) : Bool :=
  match replyVersion with
  | none => questionVersion ≤ 2
  | some version => version ≥ 3 && version == questionVersion

/-- Unknown future advertisements are not interpreted as support. A supported
new runtime keeps accepting the earlier source-bound reply contract. -/
def runtimeSupports (replyVersion runtimeVersion : Option Nat) : Bool :=
  let required := replyVersion.getD 2
  match runtimeVersion with
  | none => false
  | some version => required ≥ 2 && required ≤ version && version ≤ 3

theorem prepared_reply_needs_supported_runtime : runtimeSupports (some 3) (some 2) = false := by
  decide

theorem legacy_reply_remains_supported : runtimeSupports none (some 3) = true := by
  decide

theorem older_questions_cannot_offer_prompts (policy : Policy) (multiSelect nonempty : Bool)
    (h : policy.version ≠ 3) : promptAllowed policy multiSelect true nonempty = false := by
  simp [promptAllowed, h]

theorem suggestions_are_not_an_answer (policy : Policy) :
    accepts policy false false false = false := by
  simp [accepts]

/-- A modern question is consumed through its correlated signed reply, including
an explicit optional Skip. Generic dismissal supplies no such reply receipt. -/
def dismiss? (version : Nat) (item : Item) (principalDid : String) : Option Item :=
  if version < 2 then applyResolution? item (.dismiss principalDid) else none

theorem required_rejects_skip (policy : Policy) (selected text : Bool)
    (h : policy.optional = false) : accepts policy true selected text = false := by
  simp [accepts, h]

theorem skip_rejects_selected_answer (policy : Policy) (text : Bool) :
    accepts policy true true text = false := by
  simp [accepts]

theorem skip_rejects_text_answer (policy : Policy) (selected : Bool) :
    accepts policy true selected true = false := by
  simp [accepts]

theorem modern_question_cannot_be_dismissed (version : Nat) (item : Item)
    (principalDid : String) (h : 2 ≤ version) :
    dismiss? version item principalDid = none := by
  simp [dismiss?, Nat.not_lt.mpr h]

end Mailbox.Question
