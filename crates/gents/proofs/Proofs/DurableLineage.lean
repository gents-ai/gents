import Proofs.Basic
import Proofs.Request.CausalHop

/-!
# Durable request lineage

The database is the control plane, so logical identifiers are useful labels
but document identifiers are the authoritative edges.  This model describes
the ingest boundary for request lineage:

* logical and physical halves of an edge are either both present or absent;
* a request is a root, a session-message request (the full calling request
  and tool call edge written by `agent_new`/`agent_message`), or an
  explicitly marked request-only control continuation;
* malformed replicated rows are rejected individually, without preventing a
  later well-formed row from being considered; and
* queued steering admission retains signed raw input; canonical publication of
  the prepared message belongs to the owned execution start, not this lineage
  ingest boundary (see `QueuedSteering`).

`requestHop` is the causal hop, computed by `CausalHop.nextHop`. The edge
is provenance only; it grants no hierarchy, cascade or authority over the
calling session.
-/

namespace DurableLineage

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

/-- A bad replicated/foreign row is skipped at the ingest boundary instead of
    poisoning every valid request behind it in the watcher batch. -/
theorem malformed_head_does_not_poison
    (bad : RawLineage)
    (rest : List RawLineage)
    (hBad : admissible bad = false) :
    admissibleRows (bad :: rest) = admissibleRows rest := by
  simp [admissibleRows, hBad]

/-- A request-only control continuation of the session's own work: user
    steering, a retry, a Goal continuation or a completion wake. It keeps both
    halves of the parent request edge and no tool-call edge, at any hop,
    including zero for a top-level session. -/
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

/-- How an `agent_new`/`agent_message` is delivered: a new request to an idle,
    new or remote session, or a steering continuation of a busy local session. -/
inductive Delivery where
  | request
  | steering
  deriving DecidableEq, Repr

/-- What a delivered session message is written with. Both deliveries carry
    the full calling edge (the caller's request and tool call) and the hop
    `CausalHop.nextHop (.crossSession callerHop) own`. A steering continuation
    is additionally queued after the busy session's active request; that
    request orders the queue and is never its origin. -/
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

/-- Every delivery is an admissible session-message request that names the
    calling tool call, so its completion settles that call. -/
theorem session_message_write_names_its_caller (delivery : Delivery) (callerHop own : Nat) :
    admissible (sessionMessageWrite delivery callerHop own).lineage = true ∧
      (sessionMessageWrite delivery callerHop own).lineage.hasParentToolCallDocId = true := by
  simp [sessionMessageWrite, admissible, edgePairsCoherent, pairCoherent,
    parentShapeCoherent, depthCoherent, CausalHop.nextHop]
  omega

/-- Interrupting another agent's session — `agent_message` with
    `interrupt`, or `agent_interrupt` — is allowed in 0.20 only to the session
    that started it: the target's stored `AgentSession.provenance`, written
    once when the session is created from its first request and never
    rewritten, names a request of the caller's session. A later message into
    the session, even in the same second, cannot transfer this authority.
    `targetOriginCause` is that starting session, `none` for a root session.
    General interrupt permissions are deferred to a later release. -/
def interruptAllowed (callerSession targetSession : String)
    (targetOriginCause : Option String) : Bool :=
  CausalHop.sendTargetAllowed callerSession targetSession &&
    targetOriginCause == some callerSession

/-- A caller that did not start the target session is refused. -/
theorem non_spawner_interrupt_refused (callerSession targetSession : String)
    (targetOriginCause : Option String)
    (h : targetOriginCause ≠ some callerSession) :
    interruptAllowed callerSession targetSession targetOriginCause = false := by
  simp [interruptAllowed, CausalHop.sendTargetAllowed, h]

/-- The session that started another session may interrupt it. -/
theorem spawner_may_interrupt (callerSession targetSession : String)
    (h : targetSession ≠ callerSession) :
    interruptAllowed callerSession targetSession (some callerSession) = true := by
  simp [interruptAllowed, CausalHop.sendTargetAllowed, Ne.symm h]

/-- A root session, which no session started, cannot be interrupted by an
    agent. -/
theorem root_session_not_interruptible (callerSession targetSession : String) :
    interruptAllowed callerSession targetSession none = false := by
  simp [interruptAllowed]

end DurableLineage
