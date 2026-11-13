import Lean

namespace Identity.DelegatedFinality

open Lean

inductive Native where
  | pending | childCommitted | failureCommitted
  deriving BEq, DecidableEq, Repr

inductive Knowledge where
  | idle | inFlight | unconfirmed | confirmed | aborted
  deriving BEq, DecidableEq, Repr

structure State where
  native : Native := .pending
  knowledge : Knowledge := .idle
  started : Nat := 0
  deriving Repr

inductive Action where
  | begin | loseAck | pendingRead | childCas | otherFailureCas
  | exactReceipt (receiptMatches : Bool)
  | abortEvidence (valid : Bool)
  deriving Repr

/-- The child transaction and a competing failure CAS serialize on the same
pending tool row. A pending read does not rule out a later child commit. The
failure CAS here may be performed by a competing owner; the uncertain caller
must not itself infer permission to terminalize from a lost acknowledgement. -/
def step (s : State) : Action → State
  | .begin => if s.knowledge = .idle then { s with knowledge := .inFlight, started := s.started + 1 } else s
  | .loseAck => if s.knowledge = .inFlight then { s with knowledge := .unconfirmed } else s
  | .pendingRead => s
  | .childCas =>
      if (s.knowledge = .inFlight ∨ s.knowledge = .unconfirmed) ∧ s.native = .pending then
        { s with native := .childCommitted }
      else s
  | .otherFailureCas =>
      if s.native = .pending then { s with native := .failureCommitted } else s
  | .exactReceipt receiptMatches =>
      if s.knowledge = .unconfirmed ∧ s.native = .childCommitted ∧ receiptMatches then
        { s with knowledge := .confirmed }
      else s
  | .abortEvidence valid =>
      if s.knowledge = .unconfirmed ∧ s.native = .failureCommitted ∧ valid then
        { s with knowledge := .aborted }
      else s

def mayReportFailure (s : State) : Bool := s.knowledge == .aborted
def mayRetry (s : State) : Bool := s.knowledge == .aborted
def run (actions : List Action) : State := actions.foldl step {}

theorem pending_read_does_not_authorize_failure (s : State) :
    mayReportFailure (step s .pendingRead) = mayReportFailure s := rfl

theorem unconfirmed_cannot_report_or_replay :
    let s := run [.begin, .loseAck, .pendingRead, .begin]
    mayReportFailure s = false ∧ mayRetry s = false ∧ s.started = 1 := by decide

theorem failed_cas_excludes_child_commit :
    (run [.begin, .loseAck, .otherFailureCas, .childCas, .abortEvidence true]).native = .failureCommitted := by decide

theorem child_commit_excludes_failure_cas :
    (run [.begin, .loseAck, .childCas, .otherFailureCas, .exactReceipt true]).knowledge = .confirmed := by decide

def cases : List (String × List Action) :=
  [ ("pending-is-unconfirmed", [.begin, .loseAck, .pendingRead, .begin])
  , ("child-wins-after-pending-read", [.begin, .loseAck, .pendingRead, .childCas, .otherFailureCas, .exactReceipt true])
  , ("failure-wins-before-child", [.begin, .loseAck, .otherFailureCas, .childCas, .abortEvidence true])
  , ("child-committed-ack-lost", [.begin, .childCas, .loseAck, .exactReceipt true])
  , ("foreign-receipt", [.begin, .childCas, .loseAck, .exactReceipt false])
  , ("no-proof-of-abort", [.begin, .loseAck, .otherFailureCas, .abortEvidence false])
  , ("failure-proof-cannot-undo-child", [.begin, .childCas, .loseAck, .abortEvidence true]) ]

end Identity.DelegatedFinality
