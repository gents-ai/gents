---- MODULE SharedOwnerHotCounter ----
\* Concurrent read-modify-write increments of one document through the shared
\* transaction owner (`ConfigAccess::transact_write` on an embedded node), as
\* `shared_owner_hot_counter_has_no_lost_updates_at_c64` drives them.
\*
\* Each writer runs `Iters` increments. One increment is one owner call in
\* `TransactionMode::ConflictRetry` (transact_entry.rs `transact_write`), whose
\* attempts are, in code order:
\*   Begin   take a mutation-gate permit (gate.rs `MutationWriteGate::acquire`,
\*           a FIFO tokio semaphore of `MUTATION_WRITE_GATE_CAPACITY` = 64
\*           permits) and open a native transaction at the store's current
\*           sequence (apply_txn.rs `begin_local_owned_with_signer`,
\*           rollback.rs `begin_embedded_owned`; the permit lives in the
\*           transaction's rollback guard and is released when the attempt's
\*           transaction is dropped).
\*   Read    the callback reads the counter inside that transaction.
\*   Commit  the callback's update and receipt create are buffered; the owner
\*           commits (finalize.rs `commit_inner` -> `commit_transaction`).
\*           Regolith validates every key the transaction read or wrote
\*           against its observed sequence (engine/mod.rs `ConflictKey`,
\*           `ValidationSet`: "a value derived from a stale read is a lost
\*           update even when the bytes match"): it commits atomically when no
\*           newer version of the counter landed, and otherwise aborts with
\*           `TransactionError::Conflict`, reported as
\*           `definitely_not_committed`, cleanup not needed.
\*   Decide  Lean `TransactionReplay.disposition` (transact.rs
\*           `failure_disposition`): a conflict repeats the whole callback on a
\*           fresh transaction after `transaction_backoff` while attempts remain
\*           (`maxAttempts .conflictRetry` = 4); an exhausted series returns the
\*           conflict. A commit that timed out (`EMBEDDED_TRANSACTION_STORAGE_
\*           STEP_TIMEOUT`, finalize.rs) is a storage fault whose landing is
\*           unknown; in this mode it is never repeated
\*           (`an_unconfirmed_commit_ends_a_conflict_retry_run`) and reaches the
\*           caller as an error that is not a conflict.
\*
\* `clock` is wall time as the test observes it. The host's other load decides
\* how much of it passes between two steps of the writers; nothing in the
\* owner bounds that. `WaveDeadline` is the test's 60 s per-wave
\* `tokio::time::timeout`; it is not a property of the owner.
\*
\* Properties the owner promises its callers: `AckedApplied` (an Ok
\* increment's receipt and value are durable), `ConflictNotApplied` (a
\* returned conflict applied nothing), `NoDoubleApply` (no increment lands
\* twice), `NoLostUpdate` (the counter equals the number of commits),
\* `TestAccounting` (the test's own end-of-wave equation, when no attempt was
\* ambiguous), `AttemptBound`, and `EveryCallSettles` (every call returns,
\* under a scheduler that eventually runs every runnable task).
\*
\* REDs, one safeguard removed each: `ReadOutsideTxn` reads the counter before
\* the transaction that writes it; `StaleRetry` repeats a conflicted attempt on
\* a fresh transaction but keeps the value read by the old one;
\* `ConflictAsSuccess` acknowledges an aborted commit; `RetryAmbiguous`
\* repeats a commit whose landing is unknown; `LostWakeup` never wakes a
\* writer parked on a full gate when a permit comes free; `AbortOnOpenWriter` aborts a commit for a
\* concurrent writer that has not committed (no first-committer-wins), with
\* unbounded retries; `WaveDeadline` checks the test's wall-clock bound.
\* `Native` is one round of raw single-attempt transactions without the owner
\* (`RoundProgress`); `NativeAbortOnOpenWriter` removes first-committer-wins
\* from it and a round can end with no commit.
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  Writers,            \* the concurrent callers of one wave
  Iters,              \* increments per writer (the test's 8)
  Attempts,           \* Lean `maxAttempts .conflictRetry` (4)
  Capacity,           \* mutation-gate permits
  BoundedRetry,       \* FALSE: conflicts repeat without an attempt bound
  CanTimeout,         \* the commit phase may time out with its landing unknown
  CheckDeadline,      \* the wall clock advances (the test's deadline)
  Deadline,           \* ticks the test allows one wave
  ReadOutsideTxn,     \* RED
  StaleRetry,         \* RED
  ConflictAsSuccess,  \* RED
  RetryAmbiguous,     \* RED
  LostWakeup,         \* RED
  AbortOnOpenWriter   \* RED

ASSUME /\ Iters \in Nat \ {0}
       /\ Attempts \in Nat \ {0}
       /\ Capacity \in Nat \ {0}
       /\ Deadline \in Nat
       /\ \A b \in {BoundedRetry, CanTimeout, CheckDeadline, ReadOutsideTxn, StaleRetry,
                    ConflictAsSuccess, RetryAmbiguous, LostWakeup, AbortOnOpenWriter} :
            b \in BOOLEAN

Calls == Writers \X (1..Iters)

VARIABLES
  hist,     \* committed counter values by sequence; hist[1] is the seed 0
  permits,  \* free mutation-gate permits
  pc,       \* writer: start | parked | readout | open | read | backoff | done
  it,       \* writer: current increment
  tries,    \* writer: attempts begun in this call
  snap,     \* writer: sequence its transaction observes
  rv,       \* writer: counter value its callback read
  held,     \* writer: holds a gate permit
  applied,  \* call -> times its update and receipt landed
  outcome,  \* call -> none | ok | conflict | ambiguous
  clock

vars == <<hist, permits, pc, it, tries, snap, rv, held, applied, outcome, clock>>

Ver == Len(hist) - 1
Val == hist[Len(hist)]

Init ==
  /\ hist = <<0>>
  /\ permits = Capacity
  /\ pc = [w \in Writers |-> "start"]
  /\ it = [w \in Writers |-> 1]
  /\ tries = [w \in Writers |-> 0]
  /\ snap = [w \in Writers |-> 0]
  /\ rv = [w \in Writers |-> 0]
  /\ held = [w \in Writers |-> FALSE]
  /\ applied = [c \in Calls |-> 0]
  /\ outcome = [c \in Calls |-> "none"]
  /\ clock = 0

\* RED `ReadOutsideTxn`: the value is read from the latest commit before the
\* transaction that writes it is opened.
ReadLatest(w) ==
  /\ ReadOutsideTxn
  /\ pc[w] = "start"
  /\ rv' = [rv EXCEPT ![w] = Val]
  /\ pc' = [pc EXCEPT ![w] = "readout"]
  /\ UNCHANGED <<hist, permits, it, tries, snap, held, applied, outcome, clock>>

\* gate.rs `acquire` then `begin_embedded_owned`: a permit, then a snapshot.
Begin(w) ==
  /\ pc[w] = IF ReadOutsideTxn THEN "readout" ELSE "start"
  /\ permits > 0
  /\ permits' = permits - 1
  /\ held' = [held EXCEPT ![w] = TRUE]
  /\ snap' = [snap EXCEPT ![w] = Ver]
  /\ tries' = [tries EXCEPT ![w] = IF BoundedRetry THEN @ + 1 ELSE 1]
  /\ pc' = [pc EXCEPT ![w] = IF ReadOutsideTxn THEN "read" ELSE "open"]
  /\ UNCHANGED <<hist, it, rv, applied, outcome, clock>>

\* tokio's semaphore parks an acquirer that finds no permit and wakes it when
\* one is released. RED `LostWakeup` never wakes it.
Park(w) ==
  /\ pc[w] = IF ReadOutsideTxn THEN "readout" ELSE "start"
  /\ permits = 0
  /\ pc' = [pc EXCEPT ![w] = "parked"]
  /\ UNCHANGED <<hist, permits, it, tries, snap, rv, held, applied, outcome, clock>>

Wake(w) ==
  /\ ~LostWakeup
  /\ pc[w] = "parked"
  /\ permits > 0
  /\ pc' = [pc EXCEPT ![w] = IF ReadOutsideTxn THEN "readout" ELSE "start"]
  /\ UNCHANGED <<hist, permits, it, tries, snap, rv, held, applied, outcome, clock>>

\* The callback's `{ WriterCounter {value} }` inside the transaction.
Read(w) ==
  /\ pc[w] = "open"
  /\ rv' = [rv EXCEPT ![w] = hist[snap[w] + 1]]
  /\ pc' = [pc EXCEPT ![w] = "read"]
  /\ UNCHANGED <<hist, permits, it, tries, snap, held, applied, outcome, clock>>

Call(w) == <<w, it[w]>>

\* The call returns; the writer moves to its next increment.
Settle(w, result) ==
  /\ outcome' = [outcome EXCEPT ![Call(w)] = result]
  /\ it' = [it EXCEPT ![w] = IF @ < Iters THEN @ + 1 ELSE @]
  /\ pc' = [pc EXCEPT ![w] = IF it[w] < Iters THEN "start" ELSE "done"]
  /\ tries' = [tries EXCEPT ![w] = 0]

Land(w) ==
  /\ hist' = Append(hist, rv[w] + 1)
  /\ applied' = [applied EXCEPT ![Call(w)] = @ + 1]

Release(w) ==
  /\ permits' = permits + 1
  /\ held' = [held EXCEPT ![w] = FALSE]

\* Regolith's validation: the counter has no version newer than the snapshot.
\* RED `AbortOnOpenWriter` also aborts for another writer still holding an
\* uncommitted update of it.
Valid(w) ==
  /\ Ver = snap[w]
  /\ AbortOnOpenWriter => \A v \in Writers \ {w} : pc[v] # "read"

Commit(w) ==
  /\ pc[w] = "read"
  /\ IF Valid(w)
       THEN /\ Land(w)
            /\ Release(w)
            /\ Settle(w, "ok")
            /\ UNCHANGED <<snap, rv>>
       ELSE IF ConflictAsSuccess
       THEN /\ Release(w)
            /\ Settle(w, "ok")
            /\ UNCHANGED <<hist, applied, snap, rv>>
       ELSE \* Lean `disposition .conflictRetry .commit .conflict`: repeat
            \* while attempts remain, otherwise return the conflict.
            /\ Release(w)
            /\ UNCHANGED <<hist, applied>>
            /\ IF ~BoundedRetry \/ tries[w] < Attempts
                 THEN /\ pc' = [pc EXCEPT ![w] = "backoff"]
                      /\ UNCHANGED <<it, tries, outcome, snap, rv>>
                 ELSE /\ Settle(w, "conflict")
                      /\ UNCHANGED <<snap, rv>>
  /\ UNCHANGED clock

\* RED `AbortOnOpenWriter`: validation that rejects every transaction
\* overlapping an uncommitted writer of the key aborts two such writers
\* together, neither having committed.
MutualAbort(w, v) ==
  /\ AbortOnOpenWriter
  /\ w # v
  /\ pc[w] = "read" /\ pc[v] = "read"
  /\ LET pair == {w, v}
         again(x) == ~BoundedRetry \/ tries[x] < Attempts
     IN /\ permits' = permits + 2
        /\ held' = [x \in Writers |-> IF x \in pair THEN FALSE ELSE held[x]]
        /\ pc' = [x \in Writers |->
                    IF x \notin pair THEN pc[x]
                    ELSE IF again(x) THEN "backoff"
                    ELSE IF it[x] < Iters THEN "start" ELSE "done"]
        /\ outcome' = [c \in Calls |->
                    IF \E x \in pair : c = <<x, it[x]>> /\ ~again(x) THEN "conflict"
                    ELSE outcome[c]]
        /\ it' = [x \in Writers |->
                    IF x \in pair /\ ~again(x) /\ it[x] < Iters THEN it[x] + 1 ELSE it[x]]
        /\ tries' = [x \in Writers |-> IF x \in pair /\ ~again(x) THEN 0 ELSE tries[x]]
  /\ UNCHANGED <<hist, snap, rv, applied, clock>>

\* finalize.rs: the commit phase timed out; the native commit may or may not
\* have landed. Lean `disposition .conflictRetry .commit .storage` fails the
\* call; the test's worker then returns that error. RED `RetryAmbiguous`
\* repeats it.
CommitTimeout(w) ==
  /\ CanTimeout
  /\ pc[w] = "read"
  /\ \E lands \in {TRUE, FALSE} :
       /\ IF lands /\ Ver = snap[w]
            THEN Land(w)
            ELSE UNCHANGED <<hist, applied>>
  /\ Release(w)
  /\ IF RetryAmbiguous /\ tries[w] < Attempts
       THEN /\ pc' = [pc EXCEPT ![w] = "backoff"]
            /\ UNCHANGED <<it, tries, outcome>>
       ELSE /\ outcome' = [outcome EXCEPT ![Call(w)] = "ambiguous"]
            /\ pc' = [pc EXCEPT ![w] = "done"]
            /\ UNCHANGED <<it, tries>>
  /\ UNCHANGED <<snap, rv, clock>>

\* transact.rs: `transaction_backoff`, then a fresh attempt from begin. RED
\* `StaleRetry` reopens at the current sequence but keeps the old read.
Retry(w) ==
  /\ pc[w] = "backoff"
  /\ IF StaleRetry /\ permits > 0
       THEN /\ permits' = permits - 1
            /\ held' = [held EXCEPT ![w] = TRUE]
            /\ snap' = [snap EXCEPT ![w] = Ver]
            /\ tries' = [tries EXCEPT ![w] = IF BoundedRetry THEN @ + 1 ELSE 1]
            /\ pc' = [pc EXCEPT ![w] = "read"]
       ELSE /\ ~StaleRetry
            /\ pc' = [pc EXCEPT ![w] = "start"]
            /\ UNCHANGED <<permits, held, snap, tries>>
  /\ UNCHANGED <<hist, it, rv, applied, outcome, clock>>

AllDone == \A w \in Writers : pc[w] = "done"

\* Wall time passes while the host runs other work.
Tick ==
  /\ CheckDeadline
  /\ ~AllDone
  /\ clock <= Deadline
  /\ clock' = clock + 1
  /\ UNCHANGED <<hist, permits, pc, it, tries, snap, rv, held, applied, outcome>>

Step(w) ==
  \/ ReadLatest(w) \/ Read(w) \/ Commit(w) \/ CommitTimeout(w) \/ Retry(w)
  \/ Park(w) \/ Wake(w)

Next ==
  \/ \E w \in Writers : Step(w) \/ Begin(w)
  \/ \E w, v \in Writers : MutualAbort(w, v)
  \/ Tick

\* The runtime eventually runs every runnable task; the semaphore is FIFO, so
\* a waiter is not passed over forever while permits keep coming free.
Fairness ==
  \A w \in Writers :
    /\ WF_vars(ReadLatest(w) \/ Read(w) \/ Commit(w) \/ Retry(w))
    /\ SF_vars(Begin(w))
    /\ SF_vars(Wake(w))

Spec == Init /\ [][Next]_vars /\ Fairness

TypeOK ==
  /\ hist \in Seq(Nat)
  /\ permits \in 0..Capacity
  /\ pc \in [Writers -> {"start", "parked", "readout", "open", "read", "backoff", "done"}]
  /\ it \in [Writers -> 1..Iters]
  /\ tries \in [Writers -> Nat]
  /\ held \in [Writers -> BOOLEAN]
  /\ applied \in [Calls -> Nat]
  /\ outcome \in [Calls -> {"none", "ok", "conflict", "ambiguous"}]
  /\ clock \in Nat

AckedApplied == \A c \in Calls : outcome[c] = "ok" => applied[c] >= 1
ConflictNotApplied == \A c \in Calls : outcome[c] = "conflict" => applied[c] = 0
NoDoubleApply == \A c \in Calls : applied[c] <= 1
NoLostUpdate == Val = Ver
AttemptBound == BoundedRetry => \A w \in Writers : tries[w] <= Attempts
PermitsAccounted ==
  permits + Cardinality({w \in Writers : held[w]}) = Capacity

Acked == {c \in Calls : outcome[c] = "ok"}
Landed == {c \in Calls : applied[c] > 0}

\* The test's end-of-wave assertions: the counter equals the acknowledged
\* increments and the durable receipts are exactly those increments.
TestAccounting ==
  (AllDone /\ \A c \in Calls : outcome[c] # "ambiguous")
    => /\ Val = Cardinality(Acked)
       /\ Landed = Acked

EveryCallSettles == <>AllDone

\* A round of single-attempt transactions (`Attempts = 1`, as the native
\* `begin_transaction_guard`/`commit` experiment runs them, with no owner and
\* no gate wait) commits at least one writer: the first commit validated finds
\* no newer version, so it cannot abort. Checked with `EveryCallSettles`.
RoundProgress == AllDone => \E c \in Calls : outcome[c] = "ok"

\* The test's `tokio::time::timeout(Duration::from_secs(60), ...)` per wave.
WaveDeadline == clock > Deadline => AllDone
====
