import Proofs.Conformance.ContractCases.R6Background
import Proofs.Conformance.Contracts.Json.SessionDocuments
import Proofs.Conformance.Family
import Lean

namespace Conformance.Contracts
open Lean Conformance.ContractCases BackgroundCompletion

inductive WakeExtraRow where
  | nothing
  | newerInteractive
  | publishedSuccessor
  deriving ToJson, DecidableEq

/-- The scenario knobs of one wake-row case over the shared fixture: the row
after the failed parent, the physical parent the wake names, the successor's
physical and logical ids and creation time, a stale session observation, and
the wake's queue key. -/
structure WakeRowsInput where
  name : String
  extraRow : WakeExtraRow
  parentDoc : Nat
  successorDoc : Nat
  successorRequest : Nat
  successorCreatedAt : Nat
  staleSessionObservation : Bool
  queueKey : Option Nat
  deriving ToJson

structure WakeContextObservation where
  state : String
  origin : String
  admission : String
  retryCount : Nat
  maxRetries : Nat
  depth : Nat
  parentRequestId : Option Nat
  deadline : Option Nat
  deriving ToJson, DecidableEq

structure RequestHeadObservation where
  docId : Nat
  requestId : Nat
  state : String
  deriving ToJson, DecidableEq

structure SessionObservation where
  activity : Nat
  preview : Option String
  latest : Option RequestHeadObservation
  replyPreview : Option String
  deriving ToJson, DecidableEq

structure RowObservation where
  purpose : String
  node : Nat
  session : Nat
  requester : Option Nat
  agent : Nat
  createdAt : Nat
  head : RequestHeadObservation
  terminalizedAt : Option Nat
  deriving ToJson, DecidableEq

/-- The published successor context, the session's observation after
publication and the request rows. Redrive changes no other session field. -/
structure WakePublication where
  successorContext : WakeContextObservation
  sessionObservation : Option SessionObservation
  rows : List RowObservation
  deriving ToJson, DecidableEq

private def headOf (r : AgentSession.RequestObservation) : RequestHeadObservation :=
  { docId := r.docId, requestId := r.requestId, state := r.state.toDefraDB }

private def contextOf (c : WakeRequest) : WakeContextObservation :=
  { state := c.state.toDefraDB, origin := c.origin.toDefraDB
    admission := match c.admission with
      | .released => "released" | .waiting => "waiting"
      | .acquired => "acquired" | .executing => "executing"
    retryCount := c.retryCount, maxRetries := c.maxRetries, depth := c.depth
    parentRequestId := c.parentRequestId, deadline := c.deadline }

private def rowOf (r : AgentSession.RequestFact) : RowObservation :=
  { purpose := r.purpose.toWire, node := r.scope.node, session := r.scope.session
    requester := r.scope.requester, agent := r.agent, createdAt := r.createdAt
    head := headOf r.observed, terminalizedAt := r.terminalizedAt }

private def publicationOf (c : WakeRowsCase) : Option WakePublication :=
  c.result.map fun (context, session, rows) =>
    { successorContext := contextOf context
      sessionObservation := session.observation.map fun o =>
        { activity := o.activity, preview := o.preview, latest := o.latest.map headOf
          replyPreview := o.replyPreview }
      rows := rows.map rowOf }

example : backgroundWakeRowsCases.all (fun c =>
    c.result.all fun (_, session, _) =>
      { session with observation := c.session.observation } == c.session) = true := by
  native_decide

private def staleSession : AgentSession.Document :=
  { wakeSession with observation := some ⟨2, none, some ⟨999, 999, .processing⟩, none⟩ }

private def WakeRowsInput.ofCase (c : WakeRowsCase) : WakeRowsInput :=
  { name := c.name
    extraRow :=
      if c.rows.contains newerInteractive then .newerInteractive
      else if c.rows.contains wakeSuccessor then .publishedSuccessor
      else .nothing
    parentDoc := c.parentDoc
    successorDoc := c.successor.observed.docId
    successorRequest := c.successor.observed.requestId
    successorCreatedAt := c.successor.createdAt
    staleSessionObservation := c.session == staleSession
    queueKey := c.wake.queueKey }

private def WakeRowsInput.toCase (i : WakeRowsInput) : WakeRowsCase :=
  let wake := BackgroundCompletion.failedWakeFixture (queueKey := i.queueKey)
  { name := i.name
    session := if i.staleSessionObservation then staleSession else wakeSession
    rows := [wakeParent wake] ++ match i.extraRow with
      | .nothing => []
      | .newerInteractive => [newerInteractive]
      | .publishedSuccessor => [wakeSuccessor]
    parentDoc := i.parentDoc
    wake
    successor :=
      { wakeSuccessor with
        createdAt := i.successorCreatedAt
        observed := ⟨i.successorDoc, i.successorRequest, .pending⟩ } }

example : backgroundWakeRowsCases.all (fun c =>
    publicationOf (WakeRowsInput.toCase (WakeRowsInput.ofCase c)) == publicationOf c) = true := by
  native_decide

/-- model-production-gap: background publication still redrives a failed wake
through its pre-migration owner; no production path publishes the successor
from exact physical-parent and cross-requester authoritative rows, so these
row verdicts are checked in the model alone. -/
@[conformance_family]
def backgroundWakeRowsUnreached : Conformance.Family WakeRowsInput (Option WakePublication) where
  inputs := backgroundWakeRowsCases.map WakeRowsInput.ofCase
  model := fun i => publicationOf (WakeRowsInput.toCase i)

end Conformance.Contracts
