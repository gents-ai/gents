import Proofs.RequestExecutionLease.State

namespace RequestExecutionLease

/-!
Every authoritative read and its commit is one `step?` while holding
`Boundary.mutationWriteGate`. This models local serialization only. It does not
claim that another process, native handle, or remote merge shares the mutex.

Raw output append is the only producer write that does not rewrite the request's
explicit deadline. Exact replay is identity and never stamps fresh progress.
Canonical source closure, header publication, pending tool rows, and exact
recovery extent are composition obligations: this machine only authorizes their
generation-fenced request CAS and does not model their effects.
-/

inductive ProducerDecision where
  | closeOrRetract
  | acceptAndPublish
  | dispatch
  deriving DecidableEq, Repr

inductive Action (Generation : Type) where
  | claim (boundary : Boundary) (generation : Generation)
      (duration explicitDeadline : Time)
  | begin (boundary : Boundary) (generation : Generation)
  | appendOutput (boundary : Boundary) (generation : Generation)
  | renew (boundary : Boundary) (generation : Generation) (expectedDeadline : Time)
  /-- Authorizes the request-side CAS for a producer transaction. The canonical
  output/publication effects are deliberately absent from this world. -/
  | authorizeProducerDecision (boundary : Boundary) (generation : Generation)
      (decision : ProducerDecision)
  | socketTraffic (generation : Generation)
  | noOp (generation : Generation)
  | advanceTime (now : Time)
  /-- Voluntary relinquishment, not expiry recovery. -/
  | drop (boundary : Boundary) (generation : Generation)
  /-- Atomic authoritative reread and generation swap for expired work. -/
  | recoverExpired (boundary : Boundary) (expected fresh : Generation)
      (duration explicitDeadline : Time)
  /-- Reclaims explicitly dropped work. -/
  | recoverDropped (boundary : Boundary) (expected fresh : Generation)
      (duration explicitDeadline : Time)
  | finalize (boundary : Boundary) (generation : Generation) (outcome : Outcome)
  /-- External policy authority. It is not inactivity recovery and intentionally
  does not treat recent output as a veto. -/
  | policyRevoke (boundary : Boundary) (expected fresh : Generation) (outcome : Outcome)
  /-- Atomic expiry recovery that elects the fresh terminal winner. Canonical
  source recovery and accepted tool-row handling are composed elsewhere. -/
  | recoverExpiredAndFail (boundary : Boundary) (expected fresh : Generation)
  /-- Terminal recovery after an explicit drop. -/
  | recoverDroppedAndFail (boundary : Boundary) (expected fresh : Generation)
  deriving DecidableEq, Repr

/-- The production renewal policy is intentionally not deadline equality:
every explicit renewal advances the prior deadline by at least one tick. -/
def renewalInterval (duration : Time) : Time := max 1 (duration / 2)

def renewalDue (duration deadline : Time) : Time :=
  deadline - renewalInterval duration

def renewDeadline {Generation : Type}
    (pre : World Generation) (duration : Time) : Time := pre.now + duration

def installFresh {Generation : Type}
    (pre : World Generation) (generation : Generation)
    (duration explicitDeadline : Time) : World Generation :=
  { pre with
    lease := .active generation duration explicitDeadline
    usedGenerations := generation :: pre.usedGenerations }

def recoveryTerminal {Generation : Type}
    (pre : World Generation) (generation : Generation) : World Generation :=
  terminalize
    { pre with usedGenerations := generation :: pre.usedGenerations }
    generation .failed

def step? {Generation : Type} [DecidableEq Generation]
    (pre : World Generation) : Action Generation → Option (World Generation)
  | .claim boundary generation duration explicitDeadline =>
      match pre.lease with
      | .vacant =>
          if boundary = .mutationWriteGate ∧ pre.request = .pending ∧ duration > 0 ∧
              fresh pre generation ∧ pre.now < explicitDeadline then
            some
              { pre with
                request := .claimed
                lease := .active generation duration explicitDeadline
                usedGenerations := generation :: pre.usedGenerations }
          else none
      | _ => none
  | .begin boundary generation =>
      if admitted pre boundary generation ∧ pre.request = .claimed then
        some { pre with request := .processing }
      else none
  | .appendOutput boundary generation =>
      if admitted pre boundary generation ∧ pre.request = .processing then some pre else none
  | .renew boundary generation expectedDeadline =>
      match pre.lease with
      | .active owner duration explicitDeadline =>
          if admitted pre boundary generation ∧ explicitDeadline = expectedDeadline ∧
              renewalDue duration explicitDeadline ≤ pre.now ∧
              explicitDeadline < renewDeadline pre duration ∧
              renewableLifecycle pre.request then
            some { pre with lease :=
              (.active owner duration (renewDeadline pre duration)) }
          else none
      | _ => none
  | .authorizeProducerDecision boundary generation _ =>
      match pre.lease with
      | .active _ _ _ =>
          if admitted pre boundary generation ∧ pre.request = .processing then
            some pre
          else none
      | _ => none
  | .socketTraffic generation =>
      match pre.lease with
      | .active owner _ _ => if owner = generation then some pre else none
      | _ => none
  | .noOp generation =>
      match pre.lease with
      | .active owner _ _ => if owner = generation then some pre else none
      | _ => none
  | .advanceTime now =>
      if pre.now ≤ now then some { pre with now := now } else none
  | .drop boundary generation =>
      match pre.lease with
      | .active owner duration explicitDeadline =>
          if boundary = .mutationWriteGate ∧ owner = generation then
            some { pre with lease := .recoverable owner duration explicitDeadline }
          else none
      | _ => none
  | .recoverExpired boundary expected generation duration explicitDeadline =>
      match pre.lease with
      | .active owner _ _ =>
          if boundary = .mutationWriteGate ∧ owner = expected ∧
              effectiveExpiry pre ≤ pre.now ∧ duration > 0 ∧
              fresh pre generation ∧ pre.now < explicitDeadline then
            some (installFresh pre generation duration explicitDeadline)
          else none
      | _ => none
  | .recoverDropped boundary expected generation duration explicitDeadline =>
      match pre.lease with
      | .recoverable owner _ _ =>
          if boundary = .mutationWriteGate ∧ owner = expected ∧ duration > 0 ∧
              fresh pre generation ∧ pre.now < explicitDeadline then
            some (installFresh pre generation duration explicitDeadline)
          else none
      | _ => none
  | .finalize boundary generation outcome =>
      match pre.lease with
      | .active owner _ _ =>
          if admitted pre boundary generation ∧ canFinalize pre outcome ∧
              pre.continuationCount = 0 ∧ pre.tokenChargeCount = 0 then
            some (terminalize pre owner outcome)
          else none
      | _ => none
  | .policyRevoke boundary expected generation outcome =>
      match pre.lease with
      | .active owner _ _ =>
          if boundary = .mutationWriteGate ∧ owner = expected ∧ fresh pre generation ∧
              (outcome = .dead ∨ outcome = .superseded) ∧ canFinalize pre outcome ∧
              pre.continuationCount = 0 ∧ pre.tokenChargeCount = 0 then
            some (terminalize
              { pre with usedGenerations := generation :: pre.usedGenerations }
              generation outcome)
          else none
      | _ => none
  | .recoverExpiredAndFail boundary expected generation =>
      match pre.lease with
      | .active owner _ _ =>
          if boundary = .mutationWriteGate ∧ owner = expected ∧
              effectiveExpiry pre ≤ pre.now ∧ fresh pre generation ∧
              canFinalize pre .failed ∧ pre.continuationCount = 0 ∧
              pre.tokenChargeCount = 0 then
            some (recoveryTerminal pre generation)
          else none
      | _ => none
  | .recoverDroppedAndFail boundary expected generation =>
      match pre.lease with
      | .recoverable owner _ _ =>
          if boundary = .mutationWriteGate ∧ owner = expected ∧ fresh pre generation ∧
              canFinalize pre .failed ∧ pre.continuationCount = 0 ∧
              pre.tokenChargeCount = 0 then
            some (recoveryTerminal pre generation)
          else none
      | _ => none

def replay? {Generation : Type} [DecidableEq Generation] :
    World Generation → List (Action Generation) → Option (World Generation)
  | world, [] => some world
  | world, action :: rest =>
      match step? world action with
      | none => none
      | some next => replay? next rest

end RequestExecutionLease
