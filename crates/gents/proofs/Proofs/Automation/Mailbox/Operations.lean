import Proofs.Automation.Mailbox.Question

/-! The mailbox operations every client calls: one read of the served items,
one dismissal decision and one reply decision. A client supplies the item it
read and what the person entered; none decides admission itself. -/

namespace Mailbox.Operations

/-- What a list asks for. ACP already bounds the rows to what the actor may
read; the scope only chooses between the items still waiting and all of them. -/
inductive ListScope where
  | waiting
  | all
  deriving DecidableEq, Repr

/-- The one served-items filter. A client that shows several nodes partitions
the served rows by node; it never applies another status rule. -/
def serves (listScope : ListScope) (status : Status) : Bool :=
  match listScope with
  | .all => true
  | .waiting => !status.terminal

theorem waiting_serves_only_open (status : Status) :
    serves .waiting status = true ↔ status = .open := by
  cases status <;> simp [serves, Status.terminal]

theorem all_serves_every_status (status : Status) : serves .all status = true := rfl

/-- Served rows are shown newest first; equal creation times fall back to the
document identity so every client shows one order. -/
structure Position where
  createdAt : Nat
  docId : Nat
  deriving DecidableEq, Repr

def precedes (left right : Position) : Bool :=
  left.createdAt > right.createdAt ||
    (left.createdAt == right.createdAt && left.docId < right.docId)

theorem newer_precedes (left right : Position) (h : right.createdAt < left.createdAt) :
    precedes left right = true := by
  simp [precedes, h]

theorem precedes_is_asymmetric (left right : Position) (h : precedes left right = true) :
    precedes right left = false := by
  simp only [precedes, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] at h
  simp only [precedes, Bool.or_eq_false_iff, Bool.and_eq_false_iff, decide_eq_false_iff_not,
    beq_eq_false_iff_ne]
  omega

inductive DismissDecision where
  /-- A closed item is returned as it stands; dismissal is idempotent. -/
  | alreadyClosed
  /-- A question that requires its reply receipt closes only through an answer
  or an explicit skip (`Question.modern_question_cannot_be_dismissed`). -/
  | needsAnswer
  | dismissed
  deriving DecidableEq, Repr

def dismissDecision (status : Status) (needsAnswer : Bool) : DismissDecision :=
  if status.terminal then .alreadyClosed
  else if needsAnswer then .needsAnswer
  else .dismissed

theorem dismissal_never_closes_a_question_needing_an_answer (status : Status) :
    dismissDecision status true ≠ .dismissed := by
  cases status <;> simp [dismissDecision, Status.terminal]

theorem dismissal_closes_only_open_items (status : Status) (needsAnswer : Bool)
    (h : dismissDecision status needsAnswer = .dismissed) : status = .open := by
  cases status <;> simp_all [dismissDecision, Status.terminal]

inductive ReplyKind where
  /-- Plain text, sent as a request the item caused. -/
  | text
  /-- A question's answer: chosen options, free text or an explicit skip. -/
  | answer
  deriving DecidableEq, Repr

inductive ReplyDecision where
  | admitted
  | notOpen
  /-- The item's handling does not take a request as its reply. -/
  | notReplyable
  /-- An answer names an item that is not a question. -/
  | notAQuestion
  /-- The caller addressed another session, node or agent than the item's. -/
  | routeDiffers
  /-- The answer was written against another revision of the question. -/
  | questionChanged
  deriving DecidableEq, Repr

/-- The one reply admission. `routeMatches` holds when every part of the route
the caller stated equals the item's own, and an answer has the question's
session to go to. `questionCurrent` holds when the question needs no receipt or
the answer carries the hash of the question as the item now holds it. The
reply is always sent on the item's route, never the caller's. -/
def replyDecision (status : Status) (handling : Handling) (isQuestion : Bool)
    (replyKind : ReplyKind) (routeMatches questionCurrent : Bool) : ReplyDecision :=
  if status ≠ .open then .notOpen
  else if handling ≠ .startRequest then .notReplyable
  else if replyKind = .answer ∧ !isQuestion then .notAQuestion
  else if !routeMatches then .routeDiffers
  else if replyKind = .answer ∧ !questionCurrent then .questionChanged
  else .admitted

theorem admitted_reply_is_to_an_open_request_item (status : Status) (handling : Handling)
    (isQuestion : Bool) (replyKind : ReplyKind) (routeMatches questionCurrent : Bool)
    (h : replyDecision status handling isQuestion replyKind routeMatches questionCurrent
      = .admitted) :
    status = .open ∧ handling = .startRequest ∧ routeMatches = true := by
  cases status <;> cases handling <;> cases routeMatches <;> cases replyKind <;>
    cases isQuestion <;> cases questionCurrent <;> simp_all [replyDecision]

theorem admitted_answer_is_current (status : Status) (handling : Handling)
    (isQuestion routeMatches questionCurrent : Bool)
    (h : replyDecision status handling isQuestion .answer routeMatches questionCurrent
      = .admitted) :
    isQuestion = true ∧ questionCurrent = true := by
  cases status <;> cases handling <;> cases routeMatches <;>
    cases isQuestion <;> cases questionCurrent <;> simp_all [replyDecision]

/-- Text needs no question and no observed revision: a client with no
structured answer still reaches an open request item. -/
theorem text_reply_ignores_question_revision (status : Status) (handling : Handling)
    (isQuestion routeMatches current stale : Bool) :
    replyDecision status handling isQuestion .text routeMatches current =
      replyDecision status handling isQuestion .text routeMatches stale := by
  cases status <;> cases handling <;> cases routeMatches <;> cases isQuestion <;>
    simp [replyDecision]

end Mailbox.Operations
