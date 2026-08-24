import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

namespace SessionQueue

abbrev QueueKey := Nat

inductive QueueSource where
  | user
  | backgroundCompletion
  | steering
  | goal
  deriving DecidableEq, Repr

namespace QueueSource

def toDefraDB : QueueSource → String
  | .user => "user"
  | .backgroundCompletion => "background_completion"
  | .steering => "steering"
  | .goal => "goal"

def fromDefraDB? : String → Option QueueSource
  | "user" => some .user
  | "background_completion" => some .backgroundCompletion
  | "subagent_completion" => some .backgroundCompletion
  | "steering" => some .steering
  | "goal" => some .goal
  | _ => none

theorem fromDefraDB_toDefraDB (source : QueueSource) :
    fromDefraDB? source.toDefraDB = some source := by
  cases source <;> rfl

def automatedWakeup : QueueSource → Prop
  | .user => False
  | .backgroundCompletion => True
  | .steering => False
  | .goal => False

instance (source : QueueSource) : Decidable source.automatedWakeup :=
  match source with
  | .user => isFalse (by intro h; exact h)
  | .backgroundCompletion => isTrue trivial
  | .steering => isFalse (by intro h; exact h)
  | .goal => isFalse (by intro h; exact h)

end QueueSource

inductive QueuePolicy where
  | append
  | coalesce
  deriving DecidableEq, Repr

namespace QueuePolicy

def toDefraDB : QueuePolicy → String
  | .append => "append"
  | .coalesce => "coalesce"

def fromDefraDB? : String → Option QueuePolicy
  | "append" => some .append
  | "coalesce" => some .coalesce
  | _ => none

theorem fromDefraDB_toDefraDB (policy : QueuePolicy) :
    fromDefraDB? policy.toDefraDB = some policy := by
  cases policy <;> rfl

end QueuePolicy

structure QueueEntry where
  requestId : RequestId
  createdAt : Time
  source : QueueSource
  policy : QueuePolicy
  queueKey : Option QueueKey
  queuedAfter : Option RequestId
  deriving DecidableEq, Repr

namespace QueueEntry

def appendWellFormed (entry : QueueEntry) : Prop :=
  entry.source = .user ∨ entry.source = .steering

instance (entry : QueueEntry) : Decidable entry.appendWellFormed := by
  unfold QueueEntry.appendWellFormed
  infer_instance

def coalesceWellFormed (entry : QueueEntry) (key : QueueKey) : Prop :=
  entry.source = .backgroundCompletion ∧ entry.policy = .coalesce ∧ entry.queueKey = some key

instance (entry : QueueEntry) (key : QueueKey) : Decidable (entry.coalesceWellFormed key) := by
  unfold QueueEntry.coalesceWellFormed
  infer_instance

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

def canAppendAfter : List QueueEntry → QueueEntry → Bool
  | [], _ => true
  | existing :: rest, entry =>
      if existing.createdAt ≤ entry.createdAt then canAppendAfter rest entry else false

def CoalescedKeyMatch (entry : QueueEntry) (source : QueueSource) (key : QueueKey) : Prop :=
  entry.source = source ∧ entry.policy = .coalesce ∧ entry.queueKey = some key

instance (entry : QueueEntry) (source : QueueSource) (key : QueueKey) :
    Decidable (CoalescedKeyMatch entry source key) := by
  unfold CoalescedKeyMatch
  infer_instance

def containsCoalescedQueueKey : List QueueEntry → QueueSource → QueueKey → Bool
  | [], _, _ => false
  | entry :: rest, source, key =>
      if CoalescedKeyMatch entry source key then true else containsCoalescedQueueKey rest source key

def containsRequestId : List QueueEntry → RequestId → Bool
  | [], _ => false
  | entry :: rest, requestId =>
      if entry.requestId = requestId then true else containsRequestId rest requestId

def RequestIdFresh (s : SessionQueueState) (entry : QueueEntry) : Prop :=
  s.active ≠ some entry.requestId ∧
    entry.requestId ∉ s.terminal ∧
      containsRequestId s.pending entry.requestId = false

instance (s : SessionQueueState) (entry : QueueEntry) :
    Decidable (RequestIdFresh s entry) := by
  unfold RequestIdFresh
  infer_instance

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

def CreatedOrdered : List QueueEntry → Prop
  | [] => True
  | entry :: rest =>
      (∀ other, other ∈ rest → entry.createdAt ≤ other.createdAt) ∧
        CreatedOrdered rest

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

def appendPending (s : SessionQueueState) (entry : QueueEntry) : SessionQueueState :=
  { s with pending := s.pending ++ [entry] }

def claimHead (s : SessionQueueState) (entry : QueueEntry) (rest : List QueueEntry) :
    SessionQueueState :=
  { s with active := some entry.requestId, pending := rest }

def finishActive (s : SessionQueueState) (requestId : RequestId) : SessionQueueState :=
  { s with active := none, terminal := insert requestId s.terminal }

def drainAutomatedWakeups
    (s : SessionQueueState)
    (source : QueueSource)
    (queueKey : Option QueueKey) : SessionQueueState :=
  { s with
    pending := pendingAfterDrain source queueKey s.pending
    terminal := s.terminal ∪ drainedRequestIds source queueKey s.pending
  }

def createdOrdered (s : SessionQueueState) : Prop :=
  CreatedOrdered s.pending

def uniqueCoalescedQueueKeys (s : SessionQueueState) : Prop :=
  UniqueCoalescedQueueKeys s.pending

end SessionQueueState

end SessionQueue
