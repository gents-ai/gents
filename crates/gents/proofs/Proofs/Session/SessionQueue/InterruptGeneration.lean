import Proofs.Store.TransactionFence

/-!
# Interrupt generation

A session's interrupts are counted by a generation on one marker document per
session. The interrupt writes the marker and reads nothing it decides on; a
claim, a wake publication and a redrive each read the marker by id and record
the generation they saw on the row they write. A reader that saw a
generation an interrupt has since bumped conflicts at its commit and runs
again (`TransactionFence.pointRead`), so what each row records is the
generation current at its own commit.

What an interrupt covers is unchanged from `SessionQueue.latchInterrupt`: the
request active at its commit and the automated wakes pending at its commit;
pending user input survives as the next turn. Its cut-off is its commit, not
its scan: a wake committed after the scan and before the commit is drained.

A session interrupt reads no request, so no write of a request delays it. An
interrupt by request id also reads that request by id: a request that settles
meanwhile aborts it, rather than its bump passing a request claimed after the
one it named. Every other write of that request, such as a lease renewal,
aborts it as well and it is rebuilt from fresh reads; it lands while fewer such
writes land in one series of attempts than it has attempts, the renewal
cadence premise `InterruptLatchContention.tla` checks (its `Exhausted`
configuration drops the premise and loses the interrupt).
-/

namespace SessionQueue.InterruptGeneration

abbrev Generation := Nat

/-- The interrupt's only write: one past the generation it read. -/
def bump (current : Generation) : Generation := current + 1

/-- A running request is interrupted once the marker passes the generation its
claim recorded. -/
def runningInterrupted (recorded current : Generation) : Bool :=
  decide (recorded < current)

/-- How a request a reader observes has settled. -/
inductive Settlement where
  | unsettled
  | interrupted
  | otherwise
  deriving DecidableEq, Repr

/-- What a reader reports of a request. An unsettled request is interrupted
once the marker passes what its claim recorded. A request that settled as
interrupted was interrupted, whether a generation passed its claim or it was
interrupted before its claim. A request that settled otherwise was not active
at an interrupt that passes its record later: the marker counts the session's
interrupts, and only the request a generation covered at its commit is
interrupted by it. A running request's terminal reads the marker by id, so it
settles knowing every interrupt that committed first. -/
def observedInterrupted (settlement : Settlement) (recorded : Option Generation)
    (current : Generation) : Bool :=
  match settlement with
  | .unsettled => recorded.any (runningInterrupted · current)
  | .interrupted => true
  | .otherwise => false

/-- A request that completed before an interrupt is not reported interrupted
by it. -/
theorem settled_before_the_interrupt_is_not_interrupted (g : Generation) :
    observedInterrupted .otherwise (some g) (bump g) = false := rfl

/-- A request interrupted before its claim recorded no generation and is
reported interrupted. -/
theorem interrupted_before_claim_is_interrupted (current : Generation) :
    observedInterrupted .interrupted none current = true := rfl

/-- An unsettled request reads exactly as the running decision. -/
theorem unsettled_reads_the_running_decision (recorded current : Generation) :
    observedInterrupted .unsettled (some recorded) current =
      runningInterrupted recorded current := by
  simp [observedInterrupted]

inductive WakeAtClaim where
  | runs
  | drained
  deriving DecidableEq, Repr

/-- A wake whose publication recorded an older generation than the marker
holds when it is claimed was pending at an interrupt's commit: it is drained. -/
def wakeAtClaim (recorded current : Generation) : WakeAtClaim :=
  if recorded < current then .drained else .runs

/-- A request claimed before the interrupt committed was active at the commit,
so it is interrupted. -/
theorem claimed_then_interrupted (g : Generation) :
    runningInterrupted g (bump g) = true := by
  simp [runningInterrupted, bump]

/-- A claim that committed after the interrupt recorded the bumped generation:
the request was pending at the commit and runs as the next turn. -/
theorem interrupted_then_claimed (g : Generation) :
    runningInterrupted (bump g) (bump g) = false := by
  simp [runningInterrupted]

theorem wake_before_the_interrupt_is_drained (g : Generation) :
    wakeAtClaim g (bump g) = .drained := by
  simp [wakeAtClaim, bump]

theorem wake_after_the_interrupt_runs (g : Generation) :
    wakeAtClaim (bump g) (bump g) = .runs := by
  simp [wakeAtClaim]

/-- Later interrupts never revive what an earlier one covered. -/
theorem covered_stays_covered (recorded current later : Generation)
    (h : runningInterrupted recorded current = true) (h_later : current ≤ later) :
    runningInterrupted recorded later = true := by
  unfold runningInterrupted at h ⊢
  have : recorded < current := of_decide_eq_true h
  exact decide_eq_true (Nat.lt_of_lt_of_le this h_later)

/-- Every reader of the marker is fenced against an interrupt that commits
first: the interrupt writes the marker the reader read by id. -/
theorem a_reader_of_the_marker_yields_to_the_interrupt
    (reader interrupt : TransactionFence.Footprint) (marker : TransactionFence.Doc)
    (h_read : marker ∈ reader.pointReads) (h_write : marker ∈ interrupt.writes) :
    TransactionFence.fencedAgainst reader interrupt = true :=
  TransactionFence.a_named_fence_fences reader interrupt (.pointRead marker)
    (by simp [TransactionFence.Fence.holds, List.contains_iff_mem, h_read, h_write])

/-- The interrupt reads no document a claim, a wake publication or a terminal
writes, and writes nothing they write, so none of them aborts it: only
another interrupt of the session, which also writes the marker, does. -/
theorem nothing_but_an_interrupt_aborts_an_interrupt
    (interrupt other : TransactionFence.Footprint)
    (h_reads : ∀ doc ∈ interrupt.pointReads, doc ∉ other.writes)
    (h_writes : ∀ doc ∈ interrupt.writes, doc ∉ other.writes)
    (h_unique : interrupt.uniqueInserts = [])
    (h_scanned : interrupt.scanned = []) :
    TransactionFence.fencedAgainst interrupt other = false := by
  simp only [TransactionFence.fencedAgainst, TransactionFence.conflicts, h_unique, h_scanned,
    List.any_nil, Bool.or_false, Bool.or_eq_false_iff, List.any_eq_false,
    List.contains_iff_mem, decide_eq_true_eq]
  exact ⟨fun doc h => h_reads doc h, fun doc h => h_writes doc h⟩

end SessionQueue.InterruptGeneration
