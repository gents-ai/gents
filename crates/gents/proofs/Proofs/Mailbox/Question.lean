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
