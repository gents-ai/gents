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

A session interrupt reads no request, so no write of a request delays it; a
write of the session document does, since the marker's access check reads it
(`only_marker_and_session_writers_abort_an_interrupt`). An interrupt by
request id also reads that request by id: a request that settles
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

/-- What a claim finds when it reads its session's marker. The session is the
one named by the request's node and session id, whoever requested it: an
auxiliary request the Agent requests in a User's session records that
session's generation like any other. -/
inductive MarkerRead where
  /-- The claim read the marker by id. -/
  | found (generation : Generation)
  /-- No session of the request is readable. -/
  | noSession
  /-- The session is readable and its marker is not. -/
  | unreadable
  deriving DecidableEq, Repr

/-- The generation a claim records, or `none` when it refuses the request. A
claim that creates its request's session in its own transaction creates the
marker at generation 0 there, and no interrupt can have passed a marker that
has not committed, so it records 0. Any other claim that finds no marker
refuses: a request that ran with no recorded generation could be interrupted
by nothing, since `runningInterrupted` compares only what a claim recorded. -/
def claimRecord (read : MarkerRead) (createsSession : Bool) : Option Generation :=
  match read with
  | .found generation => some generation
  | .noSession => if createsSession then some 0 else none
  | .unreadable => none

/-- A claim that runs records the generation it read, or, creating the
session, the generation its fresh marker holds; an interrupt committing after
it passes that record. -/
theorem a_claim_that_runs_is_covered (read : MarkerRead) (createsSession : Bool)
    (g : Generation) (h : claimRecord read createsSession = some g) :
    (read = .found g ∨ (read = .noSession ∧ createsSession = true ∧ g = 0)) ∧
      runningInterrupted g (bump g) = true := by
  refine ⟨?_, by simp [runningInterrupted, bump]⟩
  cases read with
  | found generation => simp [claimRecord] at h; subst h; exact Or.inl rfl
  | noSession =>
      cases createsSession <;> simp [claimRecord] at h
      exact Or.inr ⟨rfl, rfl, h.symm⟩
  | unreadable => simp [claimRecord] at h

/-- A session whose marker its claimant cannot read runs nothing. -/
theorem an_unreadable_marker_refuses_the_claim (createsSession : Bool) :
    claimRecord .unreadable createsSession = none := rfl

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

/-- What a session interrupt's transaction does that the store validates: it
reads the marker by id and writes it, and the marker's access check reads the
session document, because the marker's read and update are the session's
(`Identity.SessionInterrupt`: `parent->read`, `parent->update`). External
premise (`a_session_write_aborts_a_session_interrupt`, gents-session): a write
of the session document committed inside an interrupt's attempt aborts that
attempt, and a write of the session's requests does not. -/
def interruptFootprint (marker session : TransactionFence.Doc) : TransactionFence.Footprint :=
  { TransactionFence.Footprint.empty with pointReads := [marker, session], writes := [marker] }

/-- A writer of the session document aborts an interrupt attempt: a claim's
session projection, a terminal's session observation, a title, a stop. -/
theorem a_session_writer_aborts_an_interrupt (marker session : TransactionFence.Doc)
    (writer : TransactionFence.Footprint) (h : session ∈ writer.writes) :
    TransactionFence.fencedAgainst (interruptFootprint marker session) writer = true := by
  simp [TransactionFence.fencedAgainst, TransactionFence.conflicts, interruptFootprint,
    TransactionFence.Footprint.empty, List.contains_iff_mem, h]

/-- Only a writer of the marker (another interrupt of the session) or of the
session document aborts an interrupt attempt; a write of a request, a wake or
any other document of the session does not. The interrupt therefore commits
within its attempts whenever fewer marker and session writes land during one
series of its attempts than it has attempts, the premise
`InterruptCoverage.tla` checks (its `InterruptReads` configuration drops it). -/
theorem only_marker_and_session_writers_abort_an_interrupt
    (marker session : TransactionFence.Doc) (other : TransactionFence.Footprint)
    (h_marker : marker ∉ other.writes) (h_session : session ∉ other.writes) :
    TransactionFence.fencedAgainst (interruptFootprint marker session) other = false := by
  simp [TransactionFence.fencedAgainst, TransactionFence.conflicts, interruptFootprint,
    TransactionFence.Footprint.empty, List.contains_iff_mem, h_marker, h_session]

end SessionQueue.InterruptGeneration
