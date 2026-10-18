import Proofs.Basic
import Proofs.Request.CausalHop

/-! Ingest boundary for durable request lineage: malformed replicated rows are
rejected individually. `requestHop` is provenance only and grants no hierarchy,
cascade or authority over the calling session. -/

namespace DurableLineage

/-- The session-message reader supplies these facts from a canonical session
visible through DefraDB ACP and the caller's current agent-target grants.
Listing and inspection use this same target predicate; neither copied session
labels nor a lineage display grants access outside its requester scope. -/
def sessionTargetAllowed (sameRequester sameNode caused allowedTarget : Bool) : Bool :=
  sameRequester && (sameNode || (caused && allowedTarget))

theorem foreign_requester_target_refused (sameNode caused allowedTarget : Bool) :
    sessionTargetAllowed false sameNode caused allowedTarget = false := by
  simp [sessionTargetAllowed]

theorem same_node_target_needs_no_lineage (caused allowedTarget : Bool) :
    sessionTargetAllowed true true caused allowedTarget = true := by
  simp [sessionTargetAllowed]

theorem remote_target_requires_current_grant (caused : Bool) :
    sessionTargetAllowed true false caused false = false := by
  simp [sessionTargetAllowed]

theorem remote_root_target_refused (allowedTarget : Bool) :
    sessionTargetAllowed true false false allowedTarget = false := by
  simp [sessionTargetAllowed]

structure RawLineage where
  hasParentRequestId : Bool
  hasParentRequestDocId : Bool
  hasParentToolCallId : Bool
  hasParentToolCallDocId : Bool
  requestHop : Nat
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
    row.requestHop > 0 ||
      (row.requestOnlyControl && row.controlAllowedAtDepthZero)
  else
    row.requestHop == 0

def admissible (row : RawLineage) : Bool :=
  edgePairsCoherent row && parentShapeCoherent row && depthCoherent row

def admissibleRows (rows : List RawLineage) : List RawLineage :=
  rows.filter admissible

theorem malformed_head_does_not_poison
    (bad : RawLineage)
    (rest : List RawLineage)
    (hBad : admissible bad = false) :
    admissibleRows (bad :: rest) = admissibleRows rest := by
  simp [admissibleRows, hBad]

def controlContinuation (requestHop : Nat) : RawLineage :=
  { hasParentRequestId := true
  , hasParentRequestDocId := true
  , hasParentToolCallId := false
  , hasParentToolCallDocId := false
  , requestHop
  , requestOnlyControl := true
  , controlAllowedAtDepthZero := true
  }

theorem control_continuation_is_admissible (depth : Nat) :
    admissible (controlContinuation depth) = true := by
  simp [admissible, edgePairsCoherent, pairCoherent, parentShapeCoherent,
    depthCoherent, controlContinuation]

inductive Delivery where
  | request
  | steering
  deriving DecidableEq, Repr

/-- Deliveries carry the full calling edge; a steering continuation is queued
after the busy session's active request, which orders the queue and is never
its origin. -/
structure SessionMessageWrite where
  lineage : RawLineage
  queuedAfterActive : Bool
  deriving DecidableEq, Repr

def sessionMessageWrite (delivery : Delivery) (callerHop own : Nat) : SessionMessageWrite :=
  { lineage :=
      { hasParentRequestId := true
      , hasParentRequestDocId := true
      , hasParentToolCallId := true
      , hasParentToolCallDocId := true
      , requestHop := CausalHop.nextHop (.crossSession callerHop) own
      , requestOnlyControl := false }
  , queuedAfterActive := delivery == .steering }

theorem session_message_write_names_its_caller (delivery : Delivery) (callerHop own : Nat) :
    admissible (sessionMessageWrite delivery callerHop own).lineage = true ∧
      (sessionMessageWrite delivery callerHop own).lineage.hasParentToolCallDocId = true := by
  simp [sessionMessageWrite, admissible, edgePairsCoherent, pairCoherent,
    parentShapeCoherent, depthCoherent, CausalHop.nextHop]
  omega

/-- Interrupt authority binds to the target session's `AgentSession.provenance`,
written once at creation and never rewritten. -/
def interruptAllowed (callerSession targetSession : String)
    (targetOriginCause : Option String) : Bool :=
  CausalHop.sendTargetAllowed callerSession targetSession &&
    targetOriginCause == some callerSession

theorem non_spawner_interrupt_refused (callerSession targetSession : String)
    (targetOriginCause : Option String)
    (h : targetOriginCause ≠ some callerSession) :
    interruptAllowed callerSession targetSession targetOriginCause = false := by
  simp [interruptAllowed, CausalHop.sendTargetAllowed, h]

theorem spawner_may_interrupt (callerSession targetSession : String)
    (h : targetSession ≠ callerSession) :
    interruptAllowed callerSession targetSession (some callerSession) = true := by
  simp [interruptAllowed, CausalHop.sendTargetAllowed, Ne.symm h]

theorem root_session_not_interruptible (callerSession targetSession : String) :
    interruptAllowed callerSession targetSession none = false := by
  simp [interruptAllowed]

end DurableLineage
