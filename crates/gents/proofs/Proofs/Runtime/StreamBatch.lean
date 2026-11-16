/-! When the stream writer flushes streamed provider output (#2131, #3803).

The owned loop records every provider text or reasoning item in the writer's
buffer, and each flush appends one permanent `AgentOutputSegment`. The writer
batches so a long reply is a bounded number of documents, but it must never
hold produced output while the provider pauses: a batched chunk is flushed by
the next write that finds the batch interval passed, or by the deadline the
drive loop waits on while no item arrives (`StreamWriter::next_flush_deadline`).
`plan` is that decision, from what the buffer gained since its last flush and
how long ago that flush was. `tla/StreamDelivery` checks the interleaving of
this decision with the provider's pause, the append and the client reader. -/

namespace StreamBatch

/-- What the buffer gained since its last flush. `firstVisible` is the first
visible content of the turn and `firstReasoning` its first reasoning; later
growth is `visible` when any visible content or progress grew, and
`reasoning` when only reasoning did. -/
inductive Growth where
  | none
  | firstVisible
  | visible
  | firstReasoning
  | reasoning
  deriving DecidableEq, Repr

/-- Time since the last flush: below the batch interval, at least the batch
interval but below the reasoning interval, or at least the reasoning
interval. -/
inductive BatchAge where
  | fresh
  | batch
  | reasoningBatch
  deriving DecidableEq, Repr

/-- The deadline the drive loop waits on: none, the batch interval after the
last flush, or `REASONING_BATCH_FACTOR` batch intervals after it. -/
inductive Timer where
  | idle
  | batch
  | reasoningBatch
  deriving DecidableEq, Repr

structure Plan where
  /-- The write that recorded this growth flushes at once. -/
  flushOnWrite : Bool
  /-- The deadline at which the drive loop flushes if no item arrives. -/
  timer : Timer
  deriving DecidableEq, Repr

/-- Reasoning-only growth batches at a multiple of the batch interval:
reasoning dominates streamed bytes for reasoning models and every flush is a
permanent document. Visible growth keeps the batch interval because a user
is reading it. -/
def interval : Growth → Timer
  | .none => .idle
  | .firstVisible | .visible => .batch
  | .firstReasoning | .reasoning => .reasoningBatch

def elapsed : BatchAge → Timer → Bool
  | _, .idle => false
  | .fresh, _ => false
  | .batch, .batch => true
  | .batch, .reasoningBatch => false
  | .reasoningBatch, _ => true

def first : Growth → Bool
  | .firstVisible | .firstReasoning => true
  | _ => false

/-- The writer's decision. The first visible bytes and the first reasoning of
a turn flush at once, so a client sees the reply start without waiting a
batch; later growth flushes on the write that finds its interval passed.
Every pending buffer arms the deadline of its interval, so output the provider
has produced is flushed without waiting for a later provider event; a buffer
with nothing pending arms none, so the drive loop does not wake for nothing. -/
def plan (growth : Growth) (age : BatchAge) : Plan :=
  let timer := interval growth
  { flushOnWrite := growth != .none && (first growth || elapsed age timer), timer }

/-- The safety form of delivery during a pause: pending output always has a
deliverer that needs no further provider event, the deadline. -/
theorem pending_output_arms_a_deadline (growth : Growth) (age : BatchAge)
    (pending : growth ≠ .none) : (plan growth age).timer ≠ .idle := by
  cases growth <;> simp_all [plan, interval]

/-- A buffer with nothing pending neither flushes nor arms a deadline. -/
theorem idle_buffer_waits_for_output (age : BatchAge) :
    plan .none age = { flushOnWrite := false, timer := .idle } := by
  cases age <;> rfl

/-- The first output of a turn reaches the store on the write that produced
it, whatever the batch clock says. -/
theorem first_output_flushes_at_once (age : BatchAge) :
    (plan .firstVisible age).flushOnWrite = true ∧
      (plan .firstReasoning age).flushOnWrite = true := by
  cases age <;> exact ⟨rfl, rfl⟩

/-- A write that arrives after its deadline flushes at once: the write and the
deadline agree on when output is due. -/
theorem write_after_deadline_flushes (growth : Growth) (age : BatchAge)
    (due : elapsed age (plan growth age).timer = true) :
    (plan growth age).flushOnWrite = true := by
  cases growth <;> cases age <;> simp_all [plan, interval, elapsed, first]

/-- Visible growth is never held longer than one batch interval. -/
theorem visible_growth_waits_at_most_one_batch (age : BatchAge) :
    (plan .visible age).timer = .batch := by
  cases age <;> rfl

end StreamBatch
