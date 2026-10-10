import Proofs.Basic
import Proofs.AgentSession
import Proofs.Scheduling
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
  origin : ExecutionOrigin := .interactive
  /-- The signed requester principal of the admitted request. One session queue
  orders every requester's requests; this is that request's own authority. -/
  requester : Option Nat := none
  /-- Abstract identity of the request-scoped execution settings the admission
  carries besides its content: agent, working directory, selected skills
  and workspace binding. Equal values run under the same configuration. -/
  turnContext : Nat := 0
  deriving DecidableEq, Repr

namespace QueueEntry

def appendWellFormed (entry : QueueEntry) : Prop :=
  entry.source = .user ∨ entry.source = .steering

instance (entry : QueueEntry) : Decidable entry.appendWellFormed := by
  unfold QueueEntry.appendWellFormed
  infer_instance

def coalesceWellFormed (entry : QueueEntry) (key : QueueKey) : Prop :=
  (entry.source = .backgroundCompletion ∨ entry.source = .goal) ∧
    entry.policy = .coalesce ∧ entry.queueKey = some key

instance (entry : QueueEntry) (key : QueueKey) : Decidable (entry.coalesceWellFormed key) := by
  unfold QueueEntry.coalesceWellFormed
  infer_instance

/-- A user message admitted while its session was busy: an interactive user
append queued behind an earlier request. Agent steering (`agent_message`),
goal and background-completion continuations, and scheduled or
trigger-sourced work are not user messages and never fold. -/
def queuedUserMessage (entry : QueueEntry) : Prop :=
  entry.source = .user ∧ entry.policy = .append ∧ entry.origin = .interactive ∧
    entry.queuedAfter.isSome

instance (entry : QueueEntry) : Decidable entry.queuedUserMessage := by
  unfold QueueEntry.queuedUserMessage
  infer_instance

/-- `candidate` may be answered by the turn `head` claims. Each folded message
keeps its own signed admission and transcript entry; folding changes only how
many turns run. The candidate must carry the head's own requester authority
and execution settings, so the folded turn runs exactly what each admission
authorized. -/
def foldsInto (head candidate : QueueEntry) : Prop :=
  head.source = .user ∧ head.origin = .interactive ∧
    candidate.queuedUserMessage ∧
    candidate.requester = head.requester ∧
    candidate.turnContext = head.turnContext

instance (head candidate : QueueEntry) : Decidable (head.foldsInto candidate) := by
  unfold QueueEntry.foldsInto
  infer_instance

def matchesAutomatedWakeup
    (entry : QueueEntry)
    (source : QueueSource)
    (queueKey : Option QueueKey) : Bool :=
  match queueKey with
  | none => false
  | some key =>
      if entry.origin = .scheduled ∧ source.automatedWakeup ∧
          entry.source = source ∧
          entry.coalesceWellFormed key then
        true
      else
        false

end QueueEntry

structure SessionQueueState where
  scope : AgentSession.Scope
  active : Option RequestId
  pending : List QueueEntry
  terminal : Finset RequestId
  /-- Messages the active claim selected to answer, in queue order, that its
  turn has not yet consumed. They wait behind the active request and return to
  the head of the queue if its turn ends without consuming them. -/
  folding : List QueueEntry := []
  deriving DecidableEq

/-- Queue execution belongs to the same exact session identity as durable sessions. -/
def SessionQueueState.sessionId (s : SessionQueueState) : SessionId := s.scope.session

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
      containsRequestId (s.folding ++ s.pending) entry.requestId = false

instance (s : SessionQueueState) (entry : QueueEntry) :
    Decidable (RequestIdFresh s entry) := by
  unfold RequestIdFresh
  infer_instance

def pendingAfterDrainMatching
    (source : QueueSource) (queueKey : Option QueueKey)
    (allowed : QueueEntry → Bool) : List QueueEntry → List QueueEntry
  | [] => []
  | entry :: rest =>
      let drainedRest := pendingAfterDrainMatching source queueKey allowed rest
      if entry.matchesAutomatedWakeup source queueKey && allowed entry then
        drainedRest
      else
        entry :: drainedRest

def drainedRequestIdsMatching
    (source : QueueSource) (queueKey : Option QueueKey)
    (allowed : QueueEntry → Bool) : List QueueEntry → Finset RequestId
  | [] => ∅
  | entry :: rest =>
      let restIds := drainedRequestIdsMatching source queueKey allowed rest
      if entry.matchesAutomatedWakeup source queueKey && allowed entry then
        insert entry.requestId restIds
      else
        restIds

def pendingAfterDrain (source : QueueSource) (queueKey : Option QueueKey)
    (entries : List QueueEntry) : List QueueEntry :=
  pendingAfterDrainMatching source queueKey (fun _ => true) entries

def drainedRequestIds (source : QueueSource) (queueKey : Option QueueKey)
    (entries : List QueueEntry) : Finset RequestId :=
  drainedRequestIdsMatching source queueKey (fun _ => true) entries

@[simp] theorem pendingAfterDrain_nil (source : QueueSource) (key : Option QueueKey) :
    pendingAfterDrain source key [] = [] := rfl

@[simp] theorem pendingAfterDrain_cons (source : QueueSource) (key : Option QueueKey)
    (entry : QueueEntry) (rest : List QueueEntry) :
    pendingAfterDrain source key (entry :: rest) =
      if entry.matchesAutomatedWakeup source key then pendingAfterDrain source key rest
      else entry :: pendingAfterDrain source key rest := by
  simp [pendingAfterDrain, pendingAfterDrainMatching]

@[simp] theorem drainedRequestIds_nil (source : QueueSource) (key : Option QueueKey) :
    drainedRequestIds source key [] = ∅ := rfl

@[simp] theorem drainedRequestIds_cons (source : QueueSource) (key : Option QueueKey)
    (entry : QueueEntry) (rest : List QueueEntry) :
    drainedRequestIds source key (entry :: rest) =
      if entry.matchesAutomatedWakeup source key then
        insert entry.requestId (drainedRequestIds source key rest)
      else drainedRequestIds source key rest := by
  simp [drainedRequestIds, drainedRequestIdsMatching]

/-- The pending messages a claim of `head` folds: the maximal run directly
behind it that folds into it and whose admission the claim transaction
verified. The run stops at the first other entry, so no entry passes one that
stays queued. Entries admitted after the claim are never part of the run. -/
def foldRun (head : QueueEntry) (admitted : List RequestId) :
    List QueueEntry → List QueueEntry
  | [] => []
  | entry :: rest =>
      if head.foldsInto entry ∧ entry.requestId ∈ admitted then
        entry :: foldRun head admitted rest
      else
        []

theorem foldRun_append_drop (head : QueueEntry) (admitted : List RequestId)
    (entries : List QueueEntry) :
    foldRun head admitted entries ++
      entries.drop (foldRun head admitted entries).length = entries := by
  induction entries with
  | nil => rfl
  | cons entry rest ih =>
      by_cases h : head.foldsInto entry ∧ entry.requestId ∈ admitted
      · simp only [foldRun, h, and_self, ↓reduceIte, List.length_cons, List.drop_succ_cons,
          List.cons_append, ih]
      · simp [foldRun, h]

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

/-- Claim `entry` and select its fold run. The claim is the cutoff: later
messages wait for the next turn. Selection is not consumption; nothing is
superseded until the turn publishes the message. -/
def claimFolding (s : SessionQueueState) (entry : QueueEntry) (rest : List QueueEntry)
    (admitted : List RequestId) : SessionQueueState :=
  let folded := foldRun entry admitted rest
  { s with
    active := some entry.requestId
    folding := folded
    pending := rest.drop folded.length
  }

/-- The active turn publishes the next selected message as its own authored
input before its first inference; that publication supersedes the message's
request. Selected messages are consumed in queue order. -/
def consumeFolded (s : SessionQueueState) (entry : QueueEntry) (rest : List QueueEntry) :
    SessionQueueState :=
  { s with folding := rest, terminal := insert entry.requestId s.terminal }

/-- A turn ending before it published a selected message did not answer it;
the message returns to the head of the queue in its original order. -/
def finishActive (s : SessionQueueState) (requestId : RequestId) : SessionQueueState :=
  { s with
    active := none
    terminal := insert requestId s.terminal
    pending := s.folding ++ s.pending
    folding := [] }

def drainAutomatedWakeups
    (s : SessionQueueState)
    (source : QueueSource)
    (queueKey : Option QueueKey) : SessionQueueState :=
  { s with
    pending := pendingAfterDrain source queueKey s.pending
    terminal := s.terminal ∪ drainedRequestIds source queueKey s.pending
  }

/-- The observed IDs are physical row identities abstracted as queue-local request IDs.
Only rows read by the latch transaction are eligible; an overlapping insertion
with the same coalescing key remains pending. -/
def drainObservedAutomatedWakeups
    (s : SessionQueueState) (source : QueueSource)
    (queueKey : Option QueueKey) (observed : List RequestId) : SessionQueueState :=
  let allowed := fun entry : QueueEntry => decide (entry.requestId ∈ observed)
  { s with
    pending := pendingAfterDrainMatching source queueKey allowed s.pending
    terminal := s.terminal ∪ drainedRequestIdsMatching source queueKey allowed s.pending
  }

end SessionQueueState

end SessionQueue
