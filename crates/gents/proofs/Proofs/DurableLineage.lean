import Proofs.Basic

/-!
# Durable request lineage

The database is the control plane, so logical identifiers are useful labels
but document identifiers are the authoritative edges.  This model describes
the ingest boundary for request lineage:

* logical and physical halves of an edge are either both present or absent;
* a request is a root, a full subagent bridge, or an explicitly marked
  request-only control continuation;
* malformed replicated rows are rejected individually, without preventing a
  later well-formed row from being considered; and
* a steering request is not publishable before its user message is durable.
-/

namespace DurableLineage

structure RawLineage where
  hasParentRequestId : Bool
  hasParentRequestDocId : Bool
  hasParentToolCallId : Bool
  hasParentToolCallDocId : Bool
  subagentDepth : Nat
  requestOnlyControl : Bool
  controlAllowedAtDepthZero : Bool := false
  deriving DecidableEq, Repr

def pairCoherent (logical physical : Bool) : Bool := logical == physical

def edgePairsCoherent (row : RawLineage) : Bool :=
  pairCoherent row.hasParentRequestId row.hasParentRequestDocId &&
    pairCoherent row.hasParentToolCallId row.hasParentToolCallDocId

def parentShapeCoherent (row : RawLineage) : Bool :=
  let root := !row.hasParentRequestId && !row.hasParentToolCallId
  let bridge := row.hasParentRequestId && row.hasParentToolCallId
  let control :=
    row.requestOnlyControl && row.hasParentRequestId && !row.hasParentToolCallId
  root || bridge || control

def depthCoherent (row : RawLineage) : Bool :=
  if row.hasParentRequestId then
    row.subagentDepth > 0 ||
      (row.requestOnlyControl && row.controlAllowedAtDepthZero)
  else
    row.subagentDepth == 0

def admissible (row : RawLineage) : Bool :=
  edgePairsCoherent row && parentShapeCoherent row && depthCoherent row

def admissibleRows (rows : List RawLineage) : List RawLineage :=
  rows.filter admissible

/-- A bad replicated/foreign row is skipped at the ingest boundary instead of
    poisoning every valid request behind it in the watcher batch. -/
theorem malformed_head_does_not_poison
    (bad : RawLineage)
    (rest : List RawLineage)
    (hBad : admissible bad = false) :
    admissibleRows (bad :: rest) = admissibleRows rest := by
  simp [admissibleRows, hBad]

def steeringContinuation (subagentDepth : Nat) : RawLineage :=
  { hasParentRequestId := true
  , hasParentRequestDocId := true
  , hasParentToolCallId := false
  , hasParentToolCallDocId := false
  , subagentDepth
  , requestOnlyControl := true
  }

/-- Steering is request-linked, not a new child spawn.  Normalization keeps
    both halves of the parent request edge and clears both halves of the old
    tool-call bridge. -/
theorem steering_continuation_is_admissible
    (depth : Nat)
    (hDepth : depth > 0) :
    admissible (steeringContinuation depth) = true := by
  simp [admissible, edgePairsCoherent, pairCoherent, parentShapeCoherent,
    depthCoherent, steeringContinuation, hDepth]

namespace SteeringPersistence

inductive Stage where
  | absent
  | messageDurable
  | requestVisible
  deriving DecidableEq, Repr

inductive Transition : Stage → Stage → Prop where
  | persistMessage : Transition .absent .messageDurable
  | publishRequest : Transition .messageDurable .requestVisible

def transitionAllowed : Stage → Stage → Bool
  | .absent, .messageDurable => true
  | .messageDurable, .requestVisible => true
  | _, _ => false

theorem transition_is_allowed
    {pre post : Stage}
    (h : Transition pre post) :
    transitionAllowed pre post = true := by
  cases h <;> rfl

/-- Publication has exactly one legal predecessor: the steering message is
    already durable (or both writes become visible atomically at commit). -/
theorem publish_requires_durable_message
    {pre : Stage}
    (h : Transition pre .requestVisible) :
    pre = .messageDurable := by
  cases h
  rfl

def requestVisibleBeforeMessageAllowed : Bool :=
  transitionAllowed .absent .requestVisible

def messageThenRequestAllowed : Bool :=
  transitionAllowed .messageDurable .requestVisible

theorem request_visible_before_message_forbidden :
    requestVisibleBeforeMessageAllowed = false := by
  rfl

theorem message_then_request_allowed :
    messageThenRequestAllowed = true := by
  rfl

end SteeringPersistence

end DurableLineage
