import Proofs.Basic

/-!
# Durable request lineage

The database is the control plane, so logical identifiers are useful labels
but document identifiers are the authoritative edges.  This model describes
the ingest boundary for request lineage:

* logical and physical halves of an edge are either both present or absent;
* a request is a root, a session-message request (the full calling request
  and tool call edge written by `create_session`/`send_message`), or an
  explicitly marked request-only control continuation;
* malformed replicated rows are rejected individually, without preventing a
  later well-formed row from being considered; and
* queued steering admission retains signed raw input; canonical publication of
  the prepared message belongs to the owned execution start, not this lineage
  ingest boundary (see `QueuedSteering`).

`subagentDepth` is the causal hop (`CausalHop`): a session-message request is
one further than its calling request and every continuation keeps its
predecessor's hop. The edge is provenance only; it grants no hierarchy,
cascade or authority over the calling session.
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
  , controlAllowedAtDepthZero := true
  }

/-- Steering is request-linked, not a new send.  Normalization keeps
    both halves of the parent request edge and clears both halves of the old
    tool-call bridge. -/
theorem steering_continuation_is_admissible
    (depth : Nat) :
    admissible (steeringContinuation depth) = true := by
  simp [admissible, edgePairsCoherent, pairCoherent, parentShapeCoherent,
    depthCoherent, steeringContinuation]

def backgroundCompletionContinuation (subagentDepth : Nat) : RawLineage :=
  { hasParentRequestId := true
  , hasParentRequestDocId := true
  , hasParentToolCallId := false
  , hasParentToolCallDocId := false
  , subagentDepth
  , requestOnlyControl := true
  , controlAllowedAtDepthZero := true
  }

/-- A background-completion wake is a control continuation, not a new
    send.  It therefore preserves the parent's depth, including
    depth zero for a top-level or goal-continuation session. -/
theorem background_completion_continuation_is_admissible
    (depth : Nat) :
    admissible (backgroundCompletionContinuation depth) = true := by
  simp [admissible, edgePairsCoherent, pairCoherent, parentShapeCoherent,
    depthCoherent, backgroundCompletionContinuation]

def goalContinuation (subagentDepth : Nat) : RawLineage :=
  { hasParentRequestId := true
  , hasParentRequestDocId := true
  , hasParentToolCallId := false
  , hasParentToolCallDocId := false
  , subagentDepth
  , requestOnlyControl := true
  , controlAllowedAtDepthZero := true
  }

/-- A durable-goal continuation preserves the hop and carries both the
    logical and physical parent request edge. It is controller work, not a new
    send. Session and behavior preservation are runtime request-
    construction obligations outside `RawLineage`. -/
theorem goal_continuation_is_admissible (depth : Nat) :
    admissible (goalContinuation depth) = true := by
  simp [admissible, edgePairsCoherent, pairCoherent, parentShapeCoherent,
    depthCoherent, goalContinuation]

end DurableLineage
