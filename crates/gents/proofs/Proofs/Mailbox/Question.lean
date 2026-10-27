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

def valid (policy : Policy) : Bool :=
  ((policy.version == 1 && !policy.optional) || policy.version == 2) &&
    ((2 ≤ policy.optionCount && policy.optionCount ≤ 4) ||
      (policy.version == 2 && policy.optionCount == 0 && policy.freeText))

def accepts (policy : Policy) (skip selected text : Bool) : Bool :=
  valid policy &&
    if skip then policy.version == 2 && policy.optional && !selected && !text
    else (!text || policy.freeText) &&
      ((!selected || policy.optionCount > 0) && (selected || text))

/-- A required modern question holds only its completed asking request's
automatic continuation. Optional prompts, other requests and cancelled work do
not hold independent work. An accepted reply closes the item through its
existing signed owner; merely showing a question grants no access. -/
def awaitingAnswer (policy : Policy) (isOpen sameParent completed : Bool) : Bool :=
  valid policy && policy.version == 2 && !policy.optional &&
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
