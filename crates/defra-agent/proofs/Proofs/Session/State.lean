import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

/-!
# Session Queue State

First-class per-session request queue vocabulary for R4a. A queue state models
one session: at most one active request, later same-session requests waiting in
`pending`, and terminal request ids retained as durable history.
-/

namespace SessionQueue

/-- Opaque queue key. Runtime encodes keys as strings, but the proof model only
    needs equality and intentionally has no empty-key inhabitant. -/
abbrev QueueKey := Nat

/-- Source attached to queue metadata. `subagentCompletion` is the automated
    wake-up source R4a coalesces and drains. `steering` is modeled separately so
    it can use append semantics until R4c gives it its own rules. -/
inductive QueueSource where
  | user
  | subagentCompletion
  | steering
  deriving DecidableEq, Repr

namespace QueueSource

/-- Persisted vocabulary in `AgentRequest.metadata.queue.source`. -/
def toDefraDB : QueueSource → String
  | .user => "user"
  | .subagentCompletion => "subagent_completion"
  | .steering => "steering"

/-- Parse persisted queue-source vocabulary. -/
def fromDefraDB? : String → Option QueueSource
  | "user" => some .user
  | "subagent_completion" => some .subagentCompletion
  | "steering" => some .steering
  | _ => none

theorem fromDefraDB_toDefraDB (source : QueueSource) :
    fromDefraDB? source.toDefraDB = some source := by
  cases source <;> rfl

/-- Runtime-created completion wake-ups that can coalesce and be drained during
    cancellation. -/
def automatedWakeup : QueueSource → Prop
  | .user => False
  | .subagentCompletion => True
  | .steering => False

instance (source : QueueSource) : Decidable source.automatedWakeup :=
  match source with
  | .user => isFalse (by intro h; exact h)
  | .subagentCompletion => isTrue trivial
  | .steering => isFalse (by intro h; exact h)

end QueueSource

/-- Queue admission policy from request metadata. -/
inductive QueuePolicy where
  | append
  | coalesce
  deriving DecidableEq, Repr

namespace QueuePolicy

/-- Persisted vocabulary in `AgentRequest.metadata.queue.policy`. -/
def toDefraDB : QueuePolicy → String
  | .append => "append"
  | .coalesce => "coalesce"

/-- Parse persisted queue-policy vocabulary. -/
def fromDefraDB? : String → Option QueuePolicy
  | "append" => some .append
  | "coalesce" => some .coalesce
  | _ => none

theorem fromDefraDB_toDefraDB (policy : QueuePolicy) :
    fromDefraDB? policy.toDefraDB = some policy := by
  cases policy <;> rfl

end QueuePolicy

/-- Pending request plus its queue metadata. -/
structure QueueEntry where
  requestId : RequestId
  createdAt : Time
  source : QueueSource
  policy : QueuePolicy
  queueKey : Option QueueKey
  queuedAfter : Option RequestId
  deriving DecidableEq, Repr

namespace QueueEntry

/-- Append entries represent distinct user/steering work. Automated completion
    wake-ups must use coalesce semantics. -/
def appendWellFormed (entry : QueueEntry) : Prop :=
  entry.source = .user ∨ entry.source = .steering

instance (entry : QueueEntry) : Decidable entry.appendWellFormed := by
  unfold QueueEntry.appendWellFormed
  infer_instance

/-- Coalesced entries are keyed automated wake-ups. -/
def coalesceWellFormed (entry : QueueEntry) (key : QueueKey) : Prop :=
  entry.source = .subagentCompletion ∧ entry.policy = .coalesce ∧ entry.queueKey = some key

instance (entry : QueueEntry) (key : QueueKey) : Decidable (entry.coalesceWellFormed key) := by
  unfold QueueEntry.coalesceWellFormed
  infer_instance

/-- Whether this pending entry matches an automated cancellation drain. -/
def matchesAutomatedWakeup
    (entry : QueueEntry)
    (source : QueueSource)
    (queueKey : Option QueueKey) : Bool :=
  match queueKey with
  | none => false
  | some key =>
      if source.automatedWakeup ∧
          entry.source = source ∧
          entry.coalesceWellFormed key then
        true
      else
        false

end QueueEntry

/-- Per-session queue state. The surrounding runtime model owns request records;
    this layer owns the single-active plus pending FIFO discipline. -/
structure SessionQueueState where
  sessionId : SessionId
  active : Option RequestId
  pending : List QueueEntry
  terminal : Finset RequestId

instance : Repr SessionQueueState where
  reprPrec s _ :=
    "{ sessionId := " ++ repr s.sessionId ++
      ", active := " ++ repr s.active ++
      ", pendingLength := " ++ repr s.pending.length ++
      ", terminalCard := " ++ repr s.terminal.card ++ " }"

/-- Existing pending entries must not be newer than a newly appended entry. -/
def canAppendAfter : List QueueEntry → QueueEntry → Bool
  | [], _ => true
  | existing :: rest, entry =>
      if existing.createdAt ≤ entry.createdAt then canAppendAfter rest entry else false

/-- A pending entry occupies a coalesced queue-key slot. -/
def CoalescedKeyMatch (entry : QueueEntry) (source : QueueSource) (key : QueueKey) : Prop :=
  entry.source = source ∧ entry.policy = .coalesce ∧ entry.queueKey = some key

instance (entry : QueueEntry) (source : QueueSource) (key : QueueKey) :
    Decidable (CoalescedKeyMatch entry source key) := by
  unfold CoalescedKeyMatch
  infer_instance

/-- Whether a pending coalesced automated wake-up already exists for a source
    and key. Append entries with the same key do not suppress coalesced wake-ups. -/
def containsCoalescedQueueKey : List QueueEntry → QueueSource → QueueKey → Bool
  | [], _, _ => false
  | entry :: rest, source, key =>
      if CoalescedKeyMatch entry source key then true else containsCoalescedQueueKey rest source key

/-- Whether a pending request id already exists. -/
def containsRequestId : List QueueEntry → RequestId → Bool
  | [], _ => false
  | entry :: rest, requestId =>
      if entry.requestId = requestId then true else containsRequestId rest requestId

/-- Newly queued request ids must be distinct from active, pending, and terminal
    ids owned by this per-session queue. -/
def RequestIdFresh (s : SessionQueueState) (entry : QueueEntry) : Prop :=
  s.active ≠ some entry.requestId ∧
    entry.requestId ∉ s.terminal ∧
      containsRequestId s.pending entry.requestId = false

instance (s : SessionQueueState) (entry : QueueEntry) :
    Decidable (RequestIdFresh s entry) := by
  unfold RequestIdFresh
  infer_instance

/-- Pending entries after draining matching automated wake-ups. -/
def pendingAfterDrain
    (source : QueueSource)
    (queueKey : Option QueueKey) : List QueueEntry → List QueueEntry
  | [] => []
  | entry :: rest =>
      let drainedRest := pendingAfterDrain source queueKey rest
      if entry.matchesAutomatedWakeup source queueKey then
        drainedRest
      else
        entry :: drainedRest

/-- Request ids terminalized by draining matching automated wake-ups. -/
def drainedRequestIds
    (source : QueueSource)
    (queueKey : Option QueueKey) : List QueueEntry → Finset RequestId
  | [] => ∅
  | entry :: rest =>
      let restIds := drainedRequestIds source queueKey rest
      if entry.matchesAutomatedWakeup source queueKey then
        insert entry.requestId restIds
      else
        restIds

/-- Pending list is ordered by `createdAt`, with the head as the earliest entry. -/
def CreatedOrdered : List QueueEntry → Prop
  | [] => True
  | entry :: rest =>
      (∀ other, other ∈ rest → entry.createdAt ≤ other.createdAt) ∧
        CreatedOrdered rest

/-- Coalesced automated wake-up keys appear at most once per source in pending. -/
def UniqueCoalescedQueueKeys : List QueueEntry → Prop
  | [] => True
  | entry :: rest =>
      (∀ source key,
        entry.source = source →
        entry.policy = .coalesce →
        entry.queueKey = some key →
        ∀ other, other ∈ rest →
          ¬ CoalescedKeyMatch other source key) ∧
        UniqueCoalescedQueueKeys rest

namespace SessionQueueState

/-- Append a new pending entry at the back of the session queue. -/
def appendPending (s : SessionQueueState) (entry : QueueEntry) : SessionQueueState :=
  { s with pending := s.pending ++ [entry] }

/-- Claim the first pending entry. The transition layer guards `active = none`. -/
def claimHead (s : SessionQueueState) (entry : QueueEntry) (rest : List QueueEntry) :
    SessionQueueState :=
  { s with active := some entry.requestId, pending := rest }

/-- Finish the active request and retain its id in terminal history. -/
def finishActive (s : SessionQueueState) (requestId : RequestId) : SessionQueueState :=
  { s with active := none, terminal := insert requestId s.terminal }

/-- Drain matching automated wake-ups, terminalizing their request ids instead
    of deleting their history. -/
def drainAutomatedWakeups
    (s : SessionQueueState)
    (source : QueueSource)
    (queueKey : Option QueueKey) : SessionQueueState :=
  { s with
    pending := pendingAfterDrain source queueKey s.pending
    terminal := s.terminal ∪ drainedRequestIds source queueKey s.pending
  }

/-- Local queue invariant: pending entries remain in created order. -/
def createdOrdered (s : SessionQueueState) : Prop :=
  CreatedOrdered s.pending

/-- Local queue-key invariant for coalesced automated wake-up keys. -/
def uniqueCoalescedQueueKeys (s : SessionQueueState) : Prop :=
  UniqueCoalescedQueueKeys s.pending

end SessionQueueState

end SessionQueue
