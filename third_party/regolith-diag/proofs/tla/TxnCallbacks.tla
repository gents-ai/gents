---- MODULE TxnCallbacks ----
\* ===========================================================================
\* THE STORY, for a reader who has never seen the code
\* ===========================================================================
\* A transaction is a batch of reads and writes that either all take effect
\* (it "commits") or none do (it "aborts"). A caller can hang three kinds of
\* callback on one (src/transaction/callbacks.rs): before_commit runs just
\* before the commit is decided and may still write; on_commit runs after
\* the transaction committed; on_abort runs after it ended without
\* committing. A database can also have "hooks" (TransactionHooks): the same
\* three points, for every transaction.
\*
\* Tiny example. A shop sends "your order is placed" from on_commit and
\* releases the reserved stock from on_abort. If both ran, or neither, or
\* on_commit ran twice, or on_commit ran before the order was safely on disk
\* and the machine then lost power, the customer would be told something
\* false. And several threads can reach for the same outcome: the owner of
\* the transaction, the thread that delivers a finished commit, and `close`,
\* which aborts whatever is still open.
\*
\* What the model shows: with two callers that each may retry once, and
\* close racing them, every callback runs exactly once, of the right kind,
\* in the right order, on the right thread, and only after the commit is on
\* disk when the database promises that. Each RED configuration plants one
\* bug, and TLC must then find the broken promise.
\*
\* THE TWO FORMS OF COMMIT, both modelled as the code has them.
\*   - The blocking commit. `transact` (src/transaction/retry.rs, `run`)
\*     calls Transaction::commit (src/transaction.rs). The call claims the
\*     transaction, runs the before_commit callbacks, joins the commit
\*     pipeline and waits inside the call for its decision and, at
\*     Immediate, for the fsync that covers its commit. Then it runs the
\*     outcome's callbacks itself, on the calling thread, and returns. There
\*     is no ticket.
\*   - commit_nowait. `transact_async` (`run_async`) calls
\*     Transaction::commit_nowait (src/transaction/nowait.rs) on the
\*     caller's queue. It runs the same way up to the pipeline's decision
\*     and never waits for an fsync. A commit that is owed none (a
\*     conflict, or Eventual durability) is done: its callbacks run on the
\*     calling thread before the call returns, and the ticket it returns is
\*     ready. A commit that is owed its fsync returns a pending
\*     CommitTicket that holds a closure to read the outcome from the
\*     commit's slot of the commit ring later (Completion::resolve_with,
\*     src/io_queue/completion.rs). When the commit has landed (it is
\*     published, or a failed sync discarded it), its queue is told; at that queue's
\*     poll, or at its drop, on the thread that holds the queue, the
\*     delivery takes the closure, decides the outcome with it, claims it
\*     with one compare-and-swap and runs the callbacks. So such a commit is
\*     resolved at delivery, on the committing thread, and never before its
\*     fsync.
\*
\* Transaction callbacks (plan 3.16, D44): before_commit, on_commit,
\* on_abort and the database-wide TransactionHooks, for transactions run by
\* `transact` (3.10) and completed on the committing thread (the queue's
\* poll or drop), by `close` or by the owner.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/Callbacks.lean:
\*   step_inv, reachable_inv   the outcome machine's invariant holds in
\*                             every reachable state
\*   exactly_once, never_both  every callback of an ended attempt ran
\*                             exactly once, of the kind its outcome names,
\*                             and none of the other kind
\*                             (AtMostOnce, ExactlyOnce here)
\*   order,                    before_commit callbacks, then the hooks'
\*   callbacks_before_hooks,   before_commit, then validation, then the
\*   commit_was_validated      transaction's outcome callbacks, then the
\*                             hooks' (CallbacksBeforeHooks here)
\*   finish_commit_runs,       the outcome callbacks run in registration
\*   finish_abort_runs         order, the hooks' after them
\*   before_writes_validated   the validated write set holds every write
\*                             the before_commit callbacks made
\*                             (NoLostUpdate here)
\*   attempts_isolated         an attempt's outcome runs only that
\*                             attempt's callbacks (AttemptIsolation here)
\*   panic_aborts              a panic in before_commit ends the attempt
\*                             aborted, with its on_abort run (PanicAborts)
\*   surviving_callbacks_break_isolation   the RED case, as a counterexample
\*   delivered_once            however many paths try to deliver a decided
\*                             outcome, one CAS lets one of them run it
\*                             (AtMostOnce here, for the delivery paths)
\*   delivered_on_queue_thread every delivery runs on the thread that holds
\*                             the commit's queue (OnCommittingThread here)
\*   resolved_after_landing,   a pending ticket's outcome is read from its
\*   resolved_once_on_queue_thread   commit's slot only once the commit has
\*                             landed, once, on the thread that holds the
\*                             commit's queue (ResolvedAfterLanding here)
\*   no_claim_delivers_twice,  the RED cases DeliverNoClaim and
\*   early_resolve_is_not_durable    CommitBeforeDurable, as
\*                             counterexamples
\* TLC checks the concurrent machine here: two transact calls, in either
\* form, two committers that decide and sync, the committing threads that
\* tell each outcome, and close, for two attempts each.
\*
\* CHECKED ON THE REAL CODE.
\*   tests/loom_io_queue.rs (loom, every schedule within its bound):
\*     a_ticket_callback_runs_exactly_once        AtMostOnce at delivery
\*     every_member_queue_gets_one_completion     a landed commit tells its
\*                                                own queue, once
\*   tests/commit_nowait.rs:
\*     at_eventual_a_commit_on_a_queue_is_ready_when_it_returns
\*                                                the commit_nowait that
\*                                                owes no fsync
\*     at_immediate_a_commit_is_visible_only_once_its_queue_lands_it
\*                                                ResolvedAfterLanding,
\*                                                CommitAfterDurable
\*     a_ticket_completes_only_at_its_own_queue_poll,
\*     the_callbacks_run_once_in_order_on_the_delivering_thread
\*                                                OnCommittingThread,
\*                                                CallbacksBeforeHooks
\*     dropping_the_queue_delivers_its_tickets_on_the_dropping_thread
\*                                                the drop path
\*   tests/callbacks_exactly_once.rs, every_transaction_ends_exactly_once
\*                                                ExactlyOnce, as a property
\*                                                test over every way a
\*                                                transaction ends
\*   src/transaction/commit_claim_tests.rs,
\*     a_sweep_in_the_middle_of_the_commit_leaves_it_to_commit
\*                                                the claim between close
\*                                                and the owner's commit
\*
\* THE DESIGN (plan 3.16).
\*   - Exactly one outcome. For every transaction, exactly one of on_commit
\*     or on_abort runs, once per registered callback, however the outcome
\*     arrives: a ticket, close or a drop.
\*   - On the committing thread (D53). A committed or conflicted outcome is
\*     told, and its callbacks run, only on the committing thread: the
\*     thread that called the commit, for a blocking commit and for a
\*     commit_nowait that was done when it returned; the thread that owns
\*     the commit's queue, for a pending ticket (at that queue's poll, or
\*     when the queue is dropped, on the dropping thread, which then holds
\*     it). Never on a committer that decided it or synced it. Close runs
\*     only the on_abort of transactions still open, on the closing thread;
\*     the owner's own rollback, drop or error runs on the owner's thread.
\*   - Order. At commit, the transaction's before_commit callbacks run, then
\*     the database hooks', then validation; a write a callback makes is
\*     part of the commit and is validated. After the outcome, the
\*     transaction's callbacks run first, then the hooks'.
\*   - Attempts. Callbacks belong to one attempt. On a conflict under
\*     `transact`, that attempt's on_abort callbacks run with the Conflict,
\*     its on_commit callbacks are dropped, and the re-run registers its own.
\*   - on_commit runs when the commit is visible, and durable at Immediate.
\*   - A panic in before_commit fails the commit with CallbackPanicked and
\*     runs on_abort.
\*   - close runs the on_abort callbacks of transactions still open, and
\*     resolves the tickets its final sync covers.
\*
\* THE MECHANISM MODELLED for exactly-once: two claims, each one CAS.
\*   - The transaction's state word: "open" is CAS'd once, to "committing"
\*     by its owner's commit or to "aborted" by a rollback, a drop, an
\*     error or close. Whoever loses the CAS runs nothing: a commit that
\*     finds the transaction aborted by close returns Closed.
\*   - The ticket's outcome word (Completion::state), for a commit_nowait
\*     that was owed an fsync: "pending" until a delivery takes the ticket's
\*     resolving closure, which only one path can get, and decides the
\*     outcome; then "decided" is CAS'd once to "delivering" by the path
\*     that delivers it, the queue's poll or the queue's drop, both on the
\*     queue holder's thread; only the winner runs callbacks.
\*
\* THE WORKLOAD. Each transaction's body writes a key of its own; its
\* before_commit callback reads a shared counter at the transaction's
\* snapshot and writes it plus one. Validation is first-committer-wins on
\* the validated write set. So the counter equals the number of commits
\* exactly when every callback write is validated.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - One callback of each kind per attempt. With any number of them,
\*     Callbacks.lean proves the outcome callbacks run in registration
\*     order (finish_commit_runs, finish_abort_runs). That before_commit
\*     callbacks run oldest first holds by construction of its runBefore
\*     step; no theorem restates it.
\*   - Rollback, drop and an Err from the closure are one transition (the
\*     owner claims "aborted" and runs on_abort); an Err from before_commit
\*     is the panic transition without the panic flag.
\*   - The pipeline decides one transaction per step; the order commits
\*     are decided, synced and published in is CommitPipeline.tla's.
\*   - Which thread runs the fsync a commit is owed (a sync round): a thread
\*     that commits with a blocking call, or the poll of a queue a
\*     commit_nowait left the round on, which claimed it as one unit. That
\*     is NonBlocking.tla's (MC_NonBlocking_Green_Commit); here some
\*     pipeline thread syncs.
\*   - The third delivery path, the commit's own fallback when its queue
\*     was dropped before the ticket was registered (it brings the commit
\*     through and delivers on the calling thread). It is the drop path with the caller
\*     as the thread that holds the queue.
\*   - The ticket's on_complete callbacks, which run after the outcome's
\*     own on the same thread and in the same claim.
\*   - A pessimistic transaction's key locks. A commit_nowait releases them
\*     when the commit lands, on the thread that lands it, and close releases
\*     those of the transactions it aborts. That is LockTable.tla's
\*     (IsolationAtLanding); no callback runs there.
\*
\* CONFIGURATIONS.
\*   MC_TxnCallbacks_Green_Immediate   one blocking commit and one
\*                                     commit_nowait, two committers, a
\*                                     pending ticket delivered by poll or
\*                                     drop, close, at Immediate: every
\*                                     invariant, and everything settles.
\*   MC_TxnCallbacks_Green_Blocking    both calls through the blocking
\*                                     commit.
\*   MC_TxnCallbacks_Green_Nowait      both through commit_nowait: two
\*                                     tickets pending at once (invariants
\*                                     only).
\*   MC_TxnCallbacks_Green_Eventual    one of each at Eventual: no ticket
\*                                     is ever pending.
\*   MC_TxnCallbacks_Red_CommitBeforeDurable  an outcome is told before the
\*                                     fsync at Immediate:
\*                                     CommitAfterDurable.
\*   MC_TxnCallbacks_Red_SkipCallbackWrites   validation uses the write
\*                                     set captured before before_commit
\*                                     ran: NoLostUpdate.
\*   MC_TxnCallbacks_Red_CallbacksSurvive     a conflicted attempt's
\*                                     callbacks stay registered for the
\*                                     re-run: AttemptIsolation.
\*   MC_TxnCallbacks_Red_DeliverNoClaim the poll and the drop deliver without
\*                                     claiming the outcome: both run it,
\*                                     AtMostOnce.
\*   MC_TxnCallbacks_Red_HelperDelivers the thread that landed a commit
\*                                     resolves and delivers its
\*                                     ticket, as before D53: the callbacks
\*                                     run off the committing thread,
\*                                     OnCommittingThread.
\*   MC_TxnCallbacks_Red_CloseNoClaim  close aborts without claiming the
\*                                     transaction, and its commit goes on:
\*                                     AtMostOnce.

\* Borrow the standard definitions of numbers, sequences (ordered lists) and finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The knobs of a run. Each configuration file (MC_TxnCallbacks_*.cfg) gives them values.
CONSTANTS
  \* The tasks that each run one `transact` call (src/transaction/retry.rs): a set of numbers, one
  \* per task.
  Owners,
  \* The threads of the commit pipeline, which decide commits and sync the log
  \* (src/engine/commit/mod.rs): a set of numbers.
  Committers,
  \* How many times `transact` runs the closure before it gives up: RetryPolicy::max_attempts
  \* (src/transaction/retry.rs).
  MaxAttempts,
  \* "Immediate" (a commit is reported only after its fsync) or "Eventual" (it is reported at once):
  \* DurabilityMode (src/options.rs).
  Durability,
  \* For each owner, the form its commit takes: "blocking" (`transact`, which calls
  \* Transaction::commit, src/transaction.rs) or "nowait" (`transact_async`, which calls
  \* Transaction::commit_nowait on its queue, src/transaction/nowait.rs). A map from owner to one
  \* of the two words.
  Form,
  \* "none" for the design, or the name of one planted bug. A RED configuration names a bug and the
  \* promise it must break.
  Mutant

\* TLC refuses a configuration that allows no attempt at all,
ASSUME MaxAttempts \in Nat \ {0}
\* or one that names no known durability,
ASSUME Durability \in {"Immediate", "Eventual"}
\* or a form of commit that does not exist,
ASSUME \A x \in Owners : Form[x] \in {"blocking", "nowait"}
\* or one that names no known bug.
ASSUME Mutant \in {"none", "CommitBeforeDurable", "SkipCallbackWrites",
                   "CallbacksSurvive", "DeliverNoClaim", "HelperDelivers",
                   "CloseNoClaim"}

\* The two ways a commit's outcome reaches its ticket, both on the thread
\* that holds the commit's queue: that queue's poll, or the queue's drop.
Paths == {"poll", "drop"}

\* Attempt numbers.
Attempts == 1..MaxAttempts

\* Configuration helpers, named in the MC_TxnCallbacks_*.cfg files.
\* Every owner calls `transact`: the blocking commit.
FormBlocking == [x \in Owners |-> "blocking"]
\* Every owner calls `transact_async`: commit_nowait on its queue.
FormNowait == [x \in Owners |-> "nowait"]
\* Owner 1 calls `transact`, every other owner `transact_async`.
FormMixed == [x \in Owners |-> IF x = 1 THEN "blocking" ELSE "nowait"]

\* The value "no owner".
NoOwner == 0
\* No real owner may be numbered 0, or "no owner" would be ambiguous.
ASSUME NoOwner \notin Owners

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The database.
  \* The sequence number of the last commit: the engine's commit counter. A number.
  seq,
  \* The sequence the write-ahead log is synced to disk through; everything at or below it survives
  \* a crash (Wal::sync_data, src/engine/wal.rs). A number.
  durable,
  \* The committed value of the shared counter key that every before_commit callback of the workload
  \* adds one to. A number.
  ctr,
  \* The sequence of the last commit that wrote the counter: what validation compares with a
  \* snapshot. A number.
  ctrSeq,
  \* TRUE once `close` has begun (RegolithEngine::is_closed, src/engine/mod.rs): no transaction
  \* begins or submits after it.
  closed,
  \* Each owner's current attempt.
  \* For each owner, the number of its current attempt, 0 before the first: the loop of `run` in
  \* src/transaction/retry.rs.
  att,
  \* For each owner, where its `transact` call is: "begin", "open" (the closure runs), "before",
  \* "hook", "submit", "committing" (inside the commit call, until the pipeline has decided),
  \* "outcb" and "outhook" (running the outcome's callbacks, then the hooks', on its own thread),
  \* "waiting" (for its ticket), "abortcb", "aborthook" or "done".
  opc,
  \* For each owner, its transaction's state word: "open", "committing" or "aborted". In the code:
  \* Claim::state, one atomic byte with OPEN, COMMITTING, ABORTED (src/transaction/claim.rs).
  ost,
  \* For each owner, the sequence its attempt reads at: the transaction's snapshot_seq
  \* (src/transaction.rs). A number.
  snap,
  \* For each owner, the counter's value as its snapshot sees it: what the callback's read returns.
  \* A number.
  readv,
  \* For each owner, the keys the attempt has written so far: "own" (the closure's key) and "ctr"
  \* (the callback's). The transaction's write buffer (src/transaction/write_buffer.rs).
  buf,
  \* For each owner, the keys validation checks for a newer commit: the write set handed to the
  \* commit (check_write, src/engine/commit/write_check.rs).
  vset,
  \* For each owner, the attempts whose callbacks its transaction holds: Callbacks in
  \* src/transaction/callbacks.rs, and the on_abort list of Claim. The design holds only the current
  \* attempt's.
  reg,
  \* For each owner, its ticket's outcome word: "none" (no ticket is waited on: a blocking commit
  \* has none), "pending" (handed out while its commit is owed an fsync), "decided",
  \* "delivering" or "ready". In the code: Completion::state with PENDING, DECIDED, DELIVERING,
  \* READY (src/io_queue/completion.rs).
  ps,
  \* For each owner, what the pipeline decided, as its commit's slot holds it: "none" yet, "commit"
  \* or "conflict". A ticket's outcome (Completion::outcome) is read from here at delivery.
  verdict,
  \* For each owner, the sequence its commit got, 0 if none: what its CommitReceipt reports
  \* (src/transaction/receipt.rs). A number.
  cseq,
  \* For each owner, where its commit is in the pipeline: "none", "queued" (submitted, not yet
  \* validated) or "decided" (its commit's slot holds its outcome).
  gs,
  \* For each owner, whether its ticket still holds the closure that reads the outcome from the
  \* commit's slot (Completion::resolve, set by resolve_with in src/transaction/nowait.rs): put
  \* there when the ticket is handed out owing an fsync, taken once at delivery.
  res,
  \* The pipeline, the committers and the closer.
  \* The submitted transactions that wait for the pipeline, oldest first: a sequence of owners.
  subq,
  \* For each committer, what it is doing: "idle", or, only under the planted bug HelperDelivers,
  \* running an outcome's "callbacks" or "hooks".
  cpc,
  \* For each committer, the owner whose outcome it is completing, or NoOwner.
  ctarget,
  \* Where `close` is: "idle" (not called), "closing" (looking for open transactions), "clcb" and
  \* "clhook" (running one's on_abort callbacks, then the hooks'), or "closed".
  clpc,
  \* The owner whose transaction the closer is aborting, or NoOwner.
  cltarget,
  \* The owners whose transactions the closer has aborted: a set.
  clseen,
  \* Ghosts, per owner and attempt.
  \* A ghost is a bookkeeping variable the program does not have; it only lets the promises be
  \* stated. This one: for each owner and attempt, how many times the attempt's on_commit callback
  \* ran.
  ranC,
  \* Ghost: for each owner and attempt, how many times the attempt's on_abort callback ran.
  ranA,
  \* Ghost: how many times the database hooks' on_commit ran for the attempt
  \* (TransactionHooks::on_commit, src/transaction/callbacks.rs).
  hookC,
  \* Ghost: how many times the database hooks' on_abort ran for the attempt.
  hookA,
  \* Ghost: TRUE once the attempt's before_commit callback ran.
  beforeRan,
  \* Ghost: TRUE once the hooks' before_commit ran for the attempt.
  hookBefore,
  \* Ghost: TRUE once the pipeline validated the attempt.
  validated,
  \* Ghost: TRUE once the attempt committed.
  committed,
  \* Ghost: TRUE once the attempt's before_commit panicked.
  panicked,
  \* Ghost: one triple <<owner, the attempt that registered the callback, the attempt whose outcome
  \* ran it>> for every callback run.
  ranFor,
  \* The delivery of each owner's outcome, per path.
  \* For each owner and delivery path ("poll" or "drop"), where that path is in delivering the
  \* owner's ticket (Completion::deliver): "idle", "decide" (it took the resolving closure),
  \* "cas" (about to claim the decided outcome), "callbacks", "hooks", or "spent" (it has had its
  \* one go at this ticket).
  dpc,
  \* A ghost: who ran each set of outcome callbacks.
  \* Ghost: one pair <<owner, who>> for every run of outcome callbacks, where who is "poll", "drop",
  \* "owner", "closer" or "committer".
  runBy

\* Every variable, so a step that changes none of them is a stutter.
vars == <<seq, durable, ctr, ctrSeq, closed, att, opc, ost, snap, readv, buf,
          vset, reg, ps, verdict, cseq, gs, res, subq, cpc, ctarget, clpc, cltarget,
          clseen, ranC, ranA, hookC, hookA, beforeRan, hookBefore, validated,
          committed, panicked, ranFor, runBy, dpc>>

\* The ghosts, grouped so steps that leave them alone say so briefly.
ghosts == <<ranC, ranA, hookC, hookA, beforeRan, hookBefore, validated,
            committed, panicked, ranFor, runBy>>
\* The owners' attempt state.
owner == <<att, opc, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res>>
\* The database words.
db == <<seq, durable, ctr, ctrSeq, closed>>
\* The committers' and the closer's state.
workers == <<subq, cpc, ctarget, clpc, cltarget, clseen, dpc>>

----------------------------------------------------------------------------
\* Helpers.

\* Set g[x][a] to v.
Set2(g, x, a, v) == [g EXCEPT ![x][a] = v]

\* Add 1 to g[x][b] for every attempt b in B.
Bump(g, x, B) == [g EXCEPT ![x] = [b \in Attempts |-> IF b \in B THEN g[x][b] + 1 ELSE g[x][b]]]

\* Run the transaction's on_commit (kind "C") or on_abort (kind "A")
\* callbacks on the thread `who`: every callback it holds runs once, and
\* each run is recorded with the attempt whose outcome it reports.
RunCallbacks(x, kind, who) ==
  \* The callbacks of the kind run, one more time each.
  /\ IF kind = "C"
       \* on_commit: the run count of every callback the transaction holds goes up by one; the
       \* on_abort counts stay.
       THEN ranC' = Bump(ranC, x, reg[x]) /\ UNCHANGED ranA
       \* on_abort: the same, for the on_abort counts.
       ELSE ranA' = Bump(ranA, x, reg[x]) /\ UNCHANGED ranC
  \* Each run is recorded with the outcome's attempt.
  /\ ranFor' = ranFor \cup {<<x, b, att[x]>> : b \in reg[x]}
  \* And with the thread that ran it.
  /\ runBy' = runBy \cup {<<x, who>>}

\* Which callbacks x's decided outcome owes: on_commit ("C") after a commit, on_abort ("A") after
\* a conflict.
KindOf(x) == IF verdict[x] = "commit" THEN "C" ELSE "A"

\* x's decided commit has landed: nothing more must reach the disk before the
\* outcome may be told. A conflict wrote nothing; at Eventual no fsync is owed; at Immediate the
\* log sync covers the commit.
Landed(x) == verdict[x] = "conflict" \/ Durability = "Eventual" \/ durable >= cseq[x]

\* Where a transact call goes once an outcome has been told: a conflict with attempts left runs
\* the closure again in a fresh attempt; a commit, or a conflict on the last attempt (Exhausted),
\* ends the call.
AfterOutcome(x) == IF verdict[x] = "conflict" /\ att[x] < MaxAttempts THEN "begin" ELSE "done"

\* Run the hooks' on_commit or on_abort for x's current attempt.
RunHook(x, kind) ==
  \* Which outcome?
  IF kind = "C"
    \* A commit: the hooks' on_commit count for the current attempt goes up by one.
    THEN hookC' = Bump(hookC, x, {att[x]}) /\ UNCHANGED hookA
    \* An abort: the hooks' on_abort count does.
    ELSE hookA' = Bump(hookA, x, {att[x]}) /\ UNCHANGED hookC

----------------------------------------------------------------------------
\* The owner: one `transact` call, attempt after attempt.

\* begin: a fresh transaction for the next attempt, with a snapshot, and
\* its three callbacks registered. After close begins, begin fails with
\* Closed and nothing is registered. Mutant CallbacksSurvive: the earlier
\* attempts' callbacks stay registered on the re-run.
Begin(x) ==
  \* When: the owner is about to begin an attempt.
  /\ opc[x] = "begin"
  \* Has close begun?
  /\ IF closed
       \* Yes: begin fails with Closed and the transact call ends.
       THEN /\ opc' = [opc EXCEPT ![x] = "done"]
            \* Nothing was registered, so nothing else of the owner changes.
            /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res, dpc>>
       \* No: this is the next attempt.
       ELSE /\ att' = [att EXCEPT ![x] = @ + 1]
            \* Its snapshot is the latest commit.
            /\ snap' = [snap EXCEPT ![x] = seq]
            \* Through that snapshot the counter reads its committed value.
            /\ readv' = [readv EXCEPT ![x] = ctr]
            \* The attempt's callbacks are registered. The design drops the earlier attempts'
            \* callbacks; the bug CallbacksSurvive keeps them.
            /\ reg' = [reg EXCEPT ![x] =
                         IF Mutant = "CallbacksSurvive" THEN @ \cup {att[x] + 1} ELSE {att[x] + 1}]
            \* The closure writes its own key.
            /\ buf' = [buf EXCEPT ![x] = {"own"}]
            \* Nothing is fixed for validation yet.
            /\ vset' = [vset EXCEPT ![x] = {}]
            \* The transaction's state word starts "open".
            /\ ost' = [ost EXCEPT ![x] = "open"]
            \* No ticket exists,
            /\ ps' = [ps EXCEPT ![x] = "none"]
            \* so none holds a resolving closure.
            /\ res' = [res EXCEPT ![x] = FALSE]
            \* No commit has been submitted.
            /\ gs' = [gs EXCEPT ![x] = "none"]
            \* Nothing is decided.
            /\ verdict' = [verdict EXCEPT ![x] = "none"]
            \* No commit sequence.
            /\ cseq' = [cseq EXCEPT ![x] = 0]
            \* Neither delivery path has had a go at this attempt's ticket.
            /\ dpc' = [dpc EXCEPT ![x] = [p \in Paths |-> "idle"]]
            \* The closure runs: the transaction is open.
            /\ opc' = [opc EXCEPT ![x] = "open"]
  \* The database, the pipeline, the closer and the ghosts do not change.
  /\ UNCHANGED <<db, subq, cpc, ctarget, clpc, cltarget, clseen, ghosts>>

\* The closure returns Ok and the owner commits: CAS the state word from
\* "open" to "committing". If close already claimed it, the commit returns
\* Closed and runs nothing: close runs the on_abort callbacks.
OwnerCommit(x) ==
  \* When: the closure returned Ok while the owner still holds its transaction.
  /\ opc[x] = "open"
  \* The CAS: is the state word still "open"?
  /\ IF ost[x] = "open"
       \* Yes: the owner claims the commit, and before_commit runs next.
       THEN ost' = [ost EXCEPT ![x] = "committing"] /\ opc' = [opc EXCEPT ![x] = "before"]
       \* No: close aborted it first. The commit returns Closed and the call ends; close runs the
       \* on_abort callbacks.
       ELSE opc' = [opc EXCEPT ![x] = "done"] /\ UNCHANGED ost
  \* Nothing else changes.
  /\ UNCHANGED <<att, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res, db, workers, ghosts>>

\* The transaction ends without committing on its owner's side: rollback,
\* a drop, or an Err from the closure (not retried). CAS "open" to
\* "aborted"; the winner runs on_abort. If close claimed it, it is done.
OwnerAbort(x) ==
  \* When: the owner gives its transaction up (a rollback, a drop, or an Err from the closure).
  /\ opc[x] = "open"
  \* The CAS: is the state word still "open"?
  /\ IF ost[x] = "open"
       \* Yes: the owner claims the abort, and runs on_abort next.
       THEN ost' = [ost EXCEPT ![x] = "aborted"] /\ opc' = [opc EXCEPT ![x] = "abortcb"]
       \* No: close already aborted it and runs its callbacks; the call ends.
       ELSE opc' = [opc EXCEPT ![x] = "done"] /\ UNCHANGED ost
  \* Nothing else changes.
  /\ UNCHANGED <<att, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res, db, workers, ghosts>>

\* The transaction's before_commit runs on the committing thread: it reads
\* the counter at the snapshot and writes it plus one, through the
\* transaction.
BeforeOk(x) ==
  \* When: the owner claimed the commit and its before_commit is next.
  /\ opc[x] = "before"
  \* The callback writes the counter through the transaction: the key joins the buffer.
  /\ buf' = [buf EXCEPT ![x] = @ \cup {"ctr"}]
  \* Ghost: this attempt's before_commit ran.
  /\ beforeRan' = Set2(beforeRan, x, att[x], TRUE)
  \* The hooks' before_commit is next.
  /\ opc' = [opc EXCEPT ![x] = "hook"]
  \* Nothing else of the owner, the database or the workers changes.
  /\ UNCHANGED <<att, ost, snap, readv, vset, reg, ps, verdict, cseq, gs, res, db, workers>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<ranC, ranA, hookC, hookA, hookBefore, validated, committed, panicked, ranFor, runBy>>

\* The transaction's before_commit panics: the commit fails with
\* CallbackPanicked and the attempt goes to on_abort.
BeforePanic(x) ==
  \* When: as BeforeOk, but this time the callback panics.
  /\ opc[x] = "before"
  \* Ghost: this attempt's before_commit ran,
  /\ beforeRan' = Set2(beforeRan, x, att[x], TRUE)
  \* and it panicked.
  /\ panicked' = Set2(panicked, x, att[x], TRUE)
  \* The commit fails; the owner runs on_abort next.
  /\ opc' = [opc EXCEPT ![x] = "abortcb"]
  \* Nothing else of the owner, the database or the workers changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res, db, workers>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<ranC, ranA, hookC, hookA, hookBefore, validated, committed, ranFor, runBy>>

\* The hooks' before_commit runs after the transaction's; then the write
\* set to validate is fixed: every write the transaction holds, the
\* callbacks' included. Mutant SkipCallbackWrites: the set captured before
\* the callbacks ran.
HookBefore(x) ==
  \* When: the transaction's own before_commit returned.
  /\ opc[x] = "hook"
  \* Ghost: the hooks' before_commit ran for this attempt.
  /\ hookBefore' = Set2(hookBefore, x, att[x], TRUE)
  \* The write set to validate is fixed now: the whole buffer. The bug SkipCallbackWrites takes only
  \* what the closure wrote.
  /\ vset' = [vset EXCEPT ![x] = IF Mutant = "SkipCallbackWrites" THEN {"own"} ELSE buf[x]]
  \* Submitting is next.
  /\ opc' = [opc EXCEPT ![x] = "submit"]
  \* Nothing else of the owner, the database or the workers changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, reg, ps, verdict, cseq, gs, res, db, workers>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<ranC, ranA, hookC, hookA, beforeRan, validated, committed, panicked, ranFor, runBy>>

\* The commit proper, in either form: the transaction joins the commit
\* pipeline, or, once close has begun, the commit fails with Closed and the
\* owner runs on_abort (one atomic admission check).
Submit(x) ==
  \* When: the callbacks that run before the commit are done.
  /\ opc[x] = "submit"
  \* The admission check: has close begun?
  /\ IF closed
       \* Yes: the commit fails with Closed and the owner runs on_abort next.
       THEN /\ opc' = [opc EXCEPT ![x] = "abortcb"]
            \* Nothing is submitted.
            /\ UNCHANGED <<subq, gs>>
       \* No: the transaction joins the back of the pipeline's queue,
       ELSE /\ subq' = Append(subq, x)
            \* as a commit not yet validated.
            /\ gs' = [gs EXCEPT ![x] = "queued"]
            \* The owner is inside its commit call until the pipeline has decided.
            /\ opc' = [opc EXCEPT ![x] = "committing"]
  \* Nothing else of the owner or the database changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, res, db>>
  \* Nor do the committers, the closer, the delivery paths or the ghosts.
  /\ UNCHANGED <<cpc, ctarget, clpc, cltarget, clseen, dpc, ghosts>>

\* The blocking commit returns (Transaction::commit, src/transaction.rs): it
\* waited inside the call for the pipeline's decision and, for a commit at
\* Immediate, for the fsync that covers it. Its outcome is told on the owner's
\* own thread, next. Mutant CommitBeforeDurable: it does not wait for the
\* fsync.
BlockingReturns(x) ==
  \* The owner called `transact`: its commit blocks.
  /\ Form[x] = "blocking"
  \* When: it is inside the commit call
  /\ opc[x] = "committing"
  \* and the pipeline has decided,
  /\ gs[x] = "decided"
  \* and the commit has landed (the bug: or not).
  /\ Landed(x) \/ Mutant = "CommitBeforeDurable"
  \* The owner runs the outcome's callbacks next, itself.
  /\ opc' = [opc EXCEPT ![x] = "outcb"]
  \* Nothing else changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res, db, workers, ghosts>>

\* commit_nowait returns (src/transaction/nowait.rs). A commit that is
\* owed no fsync (a conflict, or Eventual durability) is done: its outcome
\* is told on the owner's own thread before the call returns, and the ticket
\* is ready (Committed::Done). A commit that is owed its fsync returns a
\* ticket that holds a closure to read the outcome from its slot later
\* (Committed::Pending, Completion::resolve_with); nothing is told yet.
NowaitReturns(x) ==
  \* The owner called `transact_async`: its commit does not wait.
  /\ Form[x] = "nowait"
  \* When: it is inside the commit call
  /\ opc[x] = "committing"
  \* and the pipeline has decided.
  /\ gs[x] = "decided"
  \* Is the commit owed an fsync?
  /\ IF verdict[x] = "conflict" \/ Durability = "Eventual"
       \* No: the owner runs the outcome's callbacks next, itself,
       THEN /\ opc' = [opc EXCEPT ![x] = "outcb"]
            \* and no ticket is pending.
            /\ UNCHANGED <<ps, res>>
       \* Yes: the ticket is handed out pending,
       ELSE /\ ps' = [ps EXCEPT ![x] = "pending"]
            \* holding the closure that will read the outcome,
            /\ res' = [res EXCEPT ![x] = TRUE]
            \* and the owner waits for it.
            /\ opc' = [opc EXCEPT ![x] = "waiting"]
  \* Nothing else changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, verdict, cseq, gs, db, workers, ghosts>>

\* The owner's thread runs the callbacks of an outcome its commit call
\* knows: the transaction's on_commit after a commit, its on_abort after a
\* conflict (Transaction::finish, src/transaction/callbacks.rs).
OwnerOutcomeCallbacks(x) ==
  \* When: the owner has a decided outcome to tell.
  /\ opc[x] = "outcb"
  \* The callbacks of the outcome's kind run, recorded as run by the owner.
  /\ RunCallbacks(x, KindOf(x), "owner")
  \* The hooks are next.
  /\ opc' = [opc EXCEPT ![x] = "outhook"]
  \* Nothing else of the owner, the database or the workers changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res, db, workers>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<hookC, hookA, beforeRan, hookBefore, validated, committed, panicked>>

\* Then the hooks' outcome. A blocking commit has returned: the transact
\* call goes on. A commit_nowait has returned too, with a ticket that is
\* ready (Completion::decide, then deliver_decided, with nothing left to run).
OwnerOutcomeHook(x) ==
  \* When: the owner ran the outcome's callbacks.
  /\ opc[x] = "outhook"
  \* The hooks' outcome runs.
  /\ RunHook(x, KindOf(x))
  \* Which form of commit was it?
  /\ IF Form[x] = "nowait"
       \* commit_nowait: the ticket is ready,
       THEN /\ ps' = [ps EXCEPT ![x] = "ready"]
            \* and the owner awaits it, which returns at once.
            /\ opc' = [opc EXCEPT ![x] = "waiting"]
       \* The blocking commit: the transact call goes on, to a fresh attempt or to its end,
       ELSE /\ opc' = [opc EXCEPT ![x] = AfterOutcome(x)]
            \* and there is no ticket.
            /\ UNCHANGED ps
  \* Nothing else of the owner, the database or the workers changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, verdict, cseq, gs, res, db, workers>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<ranC, ranA, beforeRan, hookBefore, validated, committed, panicked, ranFor, runBy>>

\* The owner's thread runs the transaction's on_abort callbacks.
OwnerAbortCallbacks(x) ==
  \* When: the owner has an abort to report.
  /\ opc[x] = "abortcb"
  \* The on_abort callbacks run, recorded as run by the owner.
  /\ RunCallbacks(x, "A", "owner")
  \* The hooks' on_abort is next.
  /\ opc' = [opc EXCEPT ![x] = "aborthook"]
  \* Nothing else of the owner, the database or the workers changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res, db, workers>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<hookC, hookA, beforeRan, hookBefore, validated, committed, panicked>>

\* Then the hooks' on_abort; the transact call ends (not retried).
OwnerAbortHook(x) ==
  \* When: the owner ran its on_abort callbacks.
  /\ opc[x] = "aborthook"
  \* The hooks' on_abort runs for this attempt.
  /\ RunHook(x, "A")
  \* The transact call ends: an abort the owner chose is not retried.
  /\ opc' = [opc EXCEPT ![x] = "done"]
  \* Nothing else of the owner, the database or the workers changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res, db, workers>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<ranC, ranA, beforeRan, hookBefore, validated, committed, panicked, ranFor, runBy>>

\* A pending ticket is delivered along path p, on the thread that holds the
\* commit's queue: that queue's poll takes in the note that the commit
\* has landed, or the queue is dropped and the drop delivers what it
\* held (Completion::deliver, src/io_queue/completion.rs). The first thing
\* a delivery does is take the closure that reads the outcome from the
\* commit's slot; only one path can get it. Mutant CommitBeforeDurable: a
\* path starts before the commit has landed.
DeliverBegins(x, p) ==
  \* The owner waits for its ticket,
  /\ opc[x] = "waiting"
  \* which is not delivered yet.
  /\ ps[x] \in {"pending", "decided"}
  \* This path has not had its go.
  /\ dpc[x][p] = "idle"
  \* The commit has landed (the bug: or not).
  /\ Landed(x) \/ Mutant = "CommitBeforeDurable"
  \* Does the ticket still hold its resolving closure?
  /\ IF res[x]
       \* Yes: this path takes it,
       THEN /\ res' = [res EXCEPT ![x] = FALSE]
            \* and decides the outcome next.
            /\ dpc' = [dpc EXCEPT ![x][p] = "decide"]
       \* No: the other path took it. This one goes straight to claiming,
       ELSE /\ dpc' = [dpc EXCEPT ![x][p] = "cas"]
            \* with nothing taken.
            /\ UNCHANGED res
  \* Nothing else changes.
  /\ UNCHANGED <<db, att, opc, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, gs>>
  \* The pipeline, the committers and the closer are untouched.
  /\ UNCHANGED <<subq, cpc, ctarget, clpc, cltarget, clseen, ghosts>>

\* The path that took the closure runs it: the outcome is read from the
\* landed commit's slot and stored in the ticket (Completion::decide:
\* PENDING to DECIDED). So a commit owed an fsync is resolved at delivery,
\* on the thread that holds its queue, and never before it has landed.
DeliverDecides(x, p) ==
  \* This path took the resolving closure.
  /\ dpc[x][p] = "decide"
  \* The ticket's outcome is decided: the verdict its slot holds.
  /\ ps' = [ps EXCEPT ![x] = "decided"]
  \* Claiming it is next.
  /\ dpc' = [dpc EXCEPT ![x][p] = "cas"]
  \* Nothing else changes.
  /\ UNCHANGED <<db, att, opc, ost, snap, readv, buf, vset, reg, verdict, cseq, gs, res>>
  \* The pipeline, the committers and the closer are untouched.
  /\ UNCHANGED <<subq, cpc, ctarget, clpc, cltarget, clseen, ghosts>>

\* The path claims the decided outcome with one compare-and-swap, DECIDED
\* to DELIVERING (Completion::deliver_decided). A path that finds the
\* outcome not decided yet, or claimed already, does nothing. Mutant
\* DeliverNoClaim: no compare-and-swap, so both paths go on to deliver.
DeliverClaims(x, p) ==
  \* This path is about to claim.
  /\ dpc[x][p] = "cas"
  \* Is the outcome decided and unclaimed?
  /\ IF ps[x] = "decided"
       \* Yes: it is claimed (the bug leaves it to be claimed again),
       THEN /\ ps' = [ps EXCEPT ![x] = IF Mutant = "DeliverNoClaim" THEN @ ELSE "delivering"]
            \* and this path runs the callbacks next.
            /\ dpc' = [dpc EXCEPT ![x][p] = "callbacks"]
       \* No: this path has had its go,
       ELSE /\ dpc' = [dpc EXCEPT ![x][p] = "spent"]
            \* and changed nothing.
            /\ UNCHANGED ps
  \* Nothing else changes.
  /\ UNCHANGED <<db, att, opc, ost, snap, readv, buf, vset, reg, verdict, cseq, gs, res>>
  \* The pipeline, the committers and the closer are untouched.
  /\ UNCHANGED <<subq, cpc, ctarget, clpc, cltarget, clseen, ghosts>>

\* The delivering path runs the transaction's callbacks of the outcome's
\* kind, on the committing thread (the closure Completion::first holds,
\* which calls Ending::finish).
DeliverCallbacks(x, p) ==
  \* This path claimed the outcome.
  /\ dpc[x][p] = "callbacks"
  \* The callbacks run here, recorded as run by this path.
  /\ RunCallbacks(x, KindOf(x), p)
  \* The hooks are next.
  /\ dpc' = [dpc EXCEPT ![x][p] = "hooks"]
  \* Nothing else changes.
  /\ UNCHANGED <<db, owner, subq, cpc, ctarget, clpc, cltarget, clseen>>
  \* The other ghosts are untouched.
  /\ UNCHANGED <<hookC, hookA, beforeRan, hookBefore, validated, committed, panicked>>

\* Then the hooks', and the ticket is ready.
DeliverHooks(x, p) ==
  \* This path ran the callbacks.
  /\ dpc[x][p] = "hooks"
  \* The hooks' outcome runs.
  /\ RunHook(x, KindOf(x))
  \* The ticket is ready.
  /\ ps' = [ps EXCEPT ![x] = "ready"]
  \* The path has had its go.
  /\ dpc' = [dpc EXCEPT ![x][p] = "spent"]
  \* Nothing else changes.
  /\ UNCHANGED <<db, att, opc, ost, snap, readv, buf, vset, reg, verdict, cseq, gs, res>>
  \* The pipeline, the committers and the closer are untouched.
  /\ UNCHANGED <<subq, cpc, ctarget, clpc, cltarget, clseen>>
  \* The other ghosts are untouched.
  /\ UNCHANGED <<ranC, ranA, beforeRan, hookBefore, validated, committed, panicked, ranFor, runBy>>

\* The owner's await of its ticket returns. A commit ends the transact
\* call; a conflict re-runs the closure in a fresh attempt while attempts
\* remain (Exhausted after).
Resolve(x) ==
  \* When: the owner waits for its ticket,
  /\ opc[x] = "waiting"
  \* the ticket is ready,
  /\ ps[x] = "ready"
  \* and its thread is not inside a delivery (the queue's poll and the awaiting task share it).
  /\ \A p \in Paths : dpc[x][p] \in {"idle", "spent"}
  \* The call goes on: to a fresh attempt, or to its end.
  /\ opc' = [opc EXCEPT ![x] = AfterOutcome(x)]
  \* Nothing else changes.
  /\ UNCHANGED <<att, ost, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res, db, workers, ghosts>>

----------------------------------------------------------------------------
\* The committers: decide and sync. They never deliver an outcome: that is
\* the committing thread's (BlockingReturns, NowaitReturns, DeliverBegins).
\* Mutant HelperDelivers puts back the design before D53, where the thread
\* that landed commits completed their outcomes.

\* Validate the oldest submitted transaction, first-committer-wins on its
\* validated write set, and apply it if it commits.
Decide(c) ==
  \* When: the committer is idle
  /\ cpc[c] = "idle"
  \* and a transaction waits in the pipeline.
  /\ subq # <<>>
  \* x is the oldest submitted transaction.
  /\ LET x == Head(subq)
         \* It conflicts when the counter is in its validated write set and a commit newer than its
         \* snapshot wrote the counter.
         conflict == "ctr" \in vset[x] /\ ctrSeq > snap[x]
     \* x leaves the queue.
     IN /\ subq' = Tail(subq)
        \* Ghost: this attempt was validated.
        /\ validated' = Set2(validated, x, att[x], TRUE)
        \* Its outcome is decided: its commit's slot holds it now. No ticket knows it yet.
        /\ gs' = [gs EXCEPT ![x] = "decided"]
        \* Which way?
        /\ IF conflict
             \* A conflict: that is the verdict,
             THEN /\ verdict' = [verdict EXCEPT ![x] = "conflict"]
                  \* and the database does not change.
                  /\ UNCHANGED <<seq, ctr, ctrSeq, cseq, committed>>
             \* No conflict: the verdict is a commit.
             ELSE /\ verdict' = [verdict EXCEPT ![x] = "commit"]
                  \* It takes the next sequence number,
                  /\ seq' = seq + 1
                  \* which is the commit's sequence.
                  /\ cseq' = [cseq EXCEPT ![x] = seq + 1]
                  \* Did the attempt write the counter?
                  /\ IF "ctr" \in buf[x]
                       \* Yes: the counter becomes the value the attempt read plus one, written at
                       \* this sequence.
                       THEN ctr' = readv[x] + 1 /\ ctrSeq' = seq + 1
                       \* No: the counter is untouched.
                       ELSE UNCHANGED <<ctr, ctrSeq>>
                  \* Ghost: this attempt committed.
                  /\ committed' = Set2(committed, x, att[x], TRUE)
  \* Durability, close and the rest of the owners' state do not change.
  /\ UNCHANGED <<durable, closed, att, opc, ost, snap, readv, buf, vset, reg, ps, res>>
  \* Nor do the committers, the closer or the delivery paths.
  /\ UNCHANGED <<cpc, ctarget, clpc, cltarget, clseen, dpc>>
  \* Nor do the other ghosts.
  /\ UNCHANGED <<ranC, ranA, hookC, hookA, beforeRan, hookBefore, panicked, ranFor, runBy>>

\* A WAL sync: everything committed so far is durable, so every commit
\* written so far has landed. Which thread runs the fsync (a thread that
\* commits with a blocking call, or the poll of a queue a commit_nowait
\* left the sync round on, which claimed it) is NonBlocking.tla's; here it
\* is some pipeline thread.
Fsync(c) ==
  \* When: the committer is idle
  /\ cpc[c] = "idle"
  \* and some commit is not durable yet.
  /\ durable < seq
  \* Everything committed so far becomes durable.
  /\ durable' = seq
  \* Nothing else changes.
  /\ UNCHANGED <<seq, ctr, ctrSeq, closed, owner, workers, ghosts>>

\* Mutant HelperDelivers only: the pipeline thread that landed a commit
\* delivers its ticket itself, as before D53. It takes the resolving
\* closure, decides the outcome and claims it, to complete it on its own
\* thread.
Claim(c, x) ==
  \* Only the planted bug lets a committer deliver.
  /\ Mutant = "HelperDelivers"
  \* When: the committer is idle,
  /\ cpc[c] = "idle"
  \* the ticket is pending with its resolving closure,
  /\ ps[x] = "pending" /\ res[x]
  \* and the commit has landed.
  /\ Landed(x)
  \* The committer takes the closure,
  /\ res' = [res EXCEPT ![x] = FALSE]
  \* and decides and claims the outcome.
  /\ ps' = [ps EXCEPT ![x] = "delivering"]
  \* It runs the callbacks next,
  /\ cpc' = [cpc EXCEPT ![c] = "callbacks"]
  \* for this owner.
  /\ ctarget' = [ctarget EXCEPT ![c] = x]
  \* Nothing else of the database or the owners changes.
  /\ UNCHANGED <<db, att, opc, ost, snap, readv, buf, vset, reg, verdict, cseq, gs>>
  \* Nor do the queue, the closer, the delivery paths or the ghosts.
  /\ UNCHANGED <<subq, clpc, cltarget, clseen, dpc, ghosts>>

\* The claimer runs the transaction's callbacks of the outcome's kind.
CommitterCallbacks(c) ==
  \* When: the committer claimed an outcome.
  /\ cpc[c] = "callbacks"
  \* The callbacks of the outcome's kind run, recorded as run by a committer: the wrong thread.
  /\ LET x == ctarget[c] IN RunCallbacks(x, KindOf(x), "committer")
  \* The hooks are next.
  /\ cpc' = [cpc EXCEPT ![c] = "hooks"]
  \* Nothing else of the database, the owners or the workers changes.
  /\ UNCHANGED <<db, owner, subq, ctarget, clpc, cltarget, clseen, dpc>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<hookC, hookA, beforeRan, hookBefore, validated, committed, panicked>>

\* Then the hooks', and the ticket resolves, waking the owner.
CommitterHooks(c) ==
  \* When: the committer ran the callbacks.
  /\ cpc[c] = "hooks"
  \* x is the owner whose outcome it completes.
  /\ LET x == ctarget[c] IN
       \* The hooks' outcome runs.
       /\ RunHook(x, KindOf(x))
       \* The ticket is ready.
       /\ ps' = [ps EXCEPT ![x] = "ready"]
  \* The committer is idle again,
  /\ cpc' = [cpc EXCEPT ![c] = "idle"]
  \* with no target.
  /\ ctarget' = [ctarget EXCEPT ![c] = NoOwner]
  \* Nothing else of the database or the owners changes.
  /\ UNCHANGED <<db, att, opc, ost, snap, readv, buf, vset, reg, verdict, cseq, gs, res>>
  \* Nor do the queue, the closer or the delivery paths.
  /\ UNCHANGED <<subq, clpc, cltarget, clseen, dpc>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<ranC, ranA, beforeRan, hookBefore, validated, committed, panicked, ranFor, runBy>>

----------------------------------------------------------------------------
\* close.

\* close begins: from now on nothing begins or submits.
Close ==
  \* When: close was not called before.
  /\ clpc = "idle"
  \* From now on, begin and submit fail.
  /\ closed' = TRUE
  \* The closer starts looking for open transactions.
  /\ clpc' = "closing"
  \* Nothing else changes.
  /\ UNCHANGED <<seq, durable, ctr, ctrSeq, owner, subq, cpc, ctarget, cltarget, clseen, dpc, ghosts>>

\* close aborts a transaction still open: CAS "open" to "aborted", then
\* run its on_abort. Mutant CloseNoClaim: no CAS, so its owner may still
\* commit it.
CloseAbort(x) ==
  \* When: close is looking for open transactions,
  /\ clpc = "closing"
  \* has not handled this one,
  /\ x \notin clseen
  \* its owner still holds it (it has neither started its commit nor given it up),
  /\ opc[x] = "open"
  \* and its state word is still "open".
  /\ ost[x] = "open"
  \* The CAS: close claims the abort. The bug CloseNoClaim leaves the word "open", so the owner can
  \* still commit.
  /\ ost' = [ost EXCEPT ![x] = IF Mutant = "CloseNoClaim" THEN @ ELSE "aborted"]
  \* The closer has handled this transaction.
  /\ clseen' = clseen \cup {x}
  \* It runs the transaction's on_abort callbacks next,
  /\ clpc' = "clcb"
  \* for this owner.
  /\ cltarget' = x
  \* Nothing else of the database or the owners changes.
  /\ UNCHANGED <<db, att, opc, snap, readv, buf, vset, reg, ps, verdict, cseq, gs, res>>
  \* No delivery changes either.
  /\ UNCHANGED <<subq, cpc, ctarget, ghosts, dpc>>

\* The closer runs the aborted transaction's on_abort callbacks.
CloseCallbacks ==
  \* When: close claimed a transaction's abort.
  /\ clpc = "clcb"
  \* Its on_abort callbacks run, recorded as run by the closer.
  /\ RunCallbacks(cltarget, "A", "closer")
  \* The hooks' on_abort is next.
  /\ clpc' = "clhook"
  \* Nothing else of the database, the owners or the workers changes.
  /\ UNCHANGED <<db, owner, subq, cpc, ctarget, cltarget, clseen, dpc>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<hookC, hookA, beforeRan, hookBefore, validated, committed, panicked>>

\* Then the hooks' on_abort.
CloseHook ==
  \* When: the closer ran the callbacks.
  /\ clpc = "clhook"
  \* The hooks' on_abort runs for that transaction's attempt.
  /\ RunHook(cltarget, "A")
  \* The closer looks for the next open transaction.
  /\ clpc' = "closing"
  \* Nothing else of the database, the owners or the workers changes.
  /\ UNCHANGED <<db, owner, subq, cpc, ctarget, cltarget, clseen, dpc>>
  \* The other ghosts do not change.
  /\ UNCHANGED <<ranC, ranA, beforeRan, hookBefore, validated, committed, panicked, ranFor, runBy>>

\* No open transaction is left to abort: the final sync, and close is done.
CloseFinish ==
  \* When: close is looking for open transactions
  /\ clpc = "closing"
  \* and none is left that it has not handled.
  /\ ~\E x \in Owners : x \notin clseen /\ opc[x] = "open" /\ ost[x] = "open"
  \* The final sync: everything committed is durable.
  /\ durable' = seq
  \* close is done.
  /\ clpc' = "closed"
  \* Nothing else changes.
  /\ UNCHANGED <<seq, ctr, ctrSeq, closed, owner, subq, cpc, ctarget, cltarget, clseen, dpc, ghosts>>

----------------------------------------------------------------------------
\* The specification.

\* The start: an empty database, every owner about to begin attempt 1.
Init ==
  \* Nothing committed, nothing durable, the counter 0 and never written, the database open.
  /\ seq = 0 /\ durable = 0 /\ ctr = 0 /\ ctrSeq = 0 /\ closed = FALSE
  \* No owner has begun an attempt.
  /\ att = [x \in Owners |-> 0]
  \* Every owner is about to begin.
  /\ opc = [x \in Owners |-> "begin"]
  \* Every state word is "open".
  /\ ost = [x \in Owners |-> "open"]
  \* No snapshot yet.
  /\ snap = [x \in Owners |-> 0]
  \* Nothing read yet.
  /\ readv = [x \in Owners |-> 0]
  \* Nothing written.
  /\ buf = [x \in Owners |-> {}]
  \* Nothing to validate.
  /\ vset = [x \in Owners |-> {}]
  \* No callbacks registered.
  /\ reg = [x \in Owners |-> {}]
  \* No ticket,
  /\ ps = [x \in Owners |-> "none"]
  \* no resolving closure,
  /\ res = [x \in Owners |-> FALSE]
  \* and no commit submitted.
  /\ gs = [x \in Owners |-> "none"]
  \* Nothing decided.
  /\ verdict = [x \in Owners |-> "none"]
  \* No commit sequence.
  /\ cseq = [x \in Owners |-> 0]
  \* The pipeline's queue is empty.
  /\ subq = <<>>
  \* Every committer is idle,
  /\ cpc = [c \in Committers |-> "idle"]
  \* with no target.
  /\ ctarget = [c \in Committers |-> NoOwner]
  \* close has not been called.
  /\ clpc = "idle" /\ cltarget = NoOwner /\ clseen = {}
  \* No on_commit callback ran.
  /\ ranC = [x \in Owners |-> [a \in Attempts |-> 0]]
  \* No on_abort callback ran.
  /\ ranA = [x \in Owners |-> [a \in Attempts |-> 0]]
  \* No hook on_commit ran.
  /\ hookC = [x \in Owners |-> [a \in Attempts |-> 0]]
  \* No hook on_abort ran.
  /\ hookA = [x \in Owners |-> [a \in Attempts |-> 0]]
  \* No before_commit ran.
  /\ beforeRan = [x \in Owners |-> [a \in Attempts |-> FALSE]]
  \* No hook before_commit ran.
  /\ hookBefore = [x \in Owners |-> [a \in Attempts |-> FALSE]]
  \* Nothing was validated.
  /\ validated = [x \in Owners |-> [a \in Attempts |-> FALSE]]
  \* Nothing committed.
  /\ committed = [x \in Owners |-> [a \in Attempts |-> FALSE]]
  \* Nothing panicked.
  /\ panicked = [x \in Owners |-> [a \in Attempts |-> FALSE]]
  \* No callback run is recorded.
  /\ ranFor = {}
  \* No path is delivering anything.
  /\ dpc = [x \in Owners |-> [p \in Paths |-> "idle"]]
  \* Nobody ran any callback.
  /\ runBy = {}

\* An owner's step. Its closure ends one way or another, and its commit
\* runs to an outcome, told on its own thread: all of these happen.
OwnerStep(x) ==
  \* The steps of the closure and of the commit up to its submission,
  \/ Begin(x) \/ OwnerCommit(x) \/ OwnerAbort(x) \/ BeforeOk(x) \/ BeforePanic(x)
  \* the owner's own abort,
  \/ HookBefore(x) \/ Submit(x) \/ OwnerAbortCallbacks(x) \/ OwnerAbortHook(x)
  \* the commit call returning, in either form, and an outcome told by the call itself,
  \/ BlockingReturns(x) \/ NowaitReturns(x) \/ OwnerOutcomeCallbacks(x) \/ OwnerOutcomeHook(x)
  \* reading the ready ticket,
  \/ Resolve(x)
  \* and delivery of a pending ticket, by the queue's poll or the queue's drop.
  \/ \E p \in Paths : \/ DeliverBegins(x, p) \/ DeliverDecides(x, p) \/ DeliverClaims(x, p)
                       \* (its callbacks and its hooks)
                       \/ DeliverCallbacks(x, p) \/ DeliverHooks(x, p)

\* A committer's step.
CommitterStep(c) ==
  \* Decide a commit, sync the log, or (only under the bug HelperDelivers) claim an outcome
  \/ Decide(c) \/ Fsync(c) \/ \E x \in Owners : Claim(c, x)
  \* and complete it.
  \/ CommitterCallbacks(c) \/ CommitterHooks(c)

\* The closer's steps once close has begun.
CloserStep == (\E x \in Owners : CloseAbort(x)) \/ CloseCallbacks \/ CloseHook \/ CloseFinish

\* Every step the system can take.
Next ==
  \* An owner takes a step,
  \/ \E x \in Owners : OwnerStep(x)
  \* or a committer does,
  \/ \E c \in Committers : CommitterStep(c)
  \* or close is called, or the closer takes a step.
  \/ Close \/ CloserStep

\* Fairness: owners, committers and a started close all make progress.
\* Calling close at all is the caller's choice.
Fairness ==
  \* Each owner that can take a step eventually takes one.
  /\ \A x \in Owners : WF_vars(OwnerStep(x))
  \* So does each committer.
  /\ \A c \in Committers : WF_vars(CommitterStep(c))
  \* So does a close that has begun.
  /\ WF_vars(CloserStep)

\* Every behaviour: start in Init, take Next steps or stutter, fairly.
Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* Attempt numbers stay within the policy.
  /\ att \in [Owners -> 0..MaxAttempts]
  \* Each state word is one of its three values.
  /\ ost \in [Owners -> {"open", "committing", "aborted"}]
  \* Each ticket's outcome word is one of its five values.
  /\ ps \in [Owners -> {"none", "pending", "decided", "delivering", "ready"}]
  \* Each commit is nowhere in the pipeline, queued, or decided.
  /\ gs \in [Owners -> {"none", "queued", "decided"}]
  \* A ticket holds its resolving closure or it does not.
  /\ res \in [Owners -> BOOLEAN]
  \* Each verdict is one of its three values.
  /\ verdict \in [Owners -> {"none", "commit", "conflict"}]
  \* Each committer is idle or completing an outcome.
  /\ cpc \in [Committers -> {"idle", "callbacks", "hooks"}]
  \* The closer is in one of its five places.
  /\ clpc \in {"idle", "closing", "clcb", "clhook", "closed"}
  \* Each delivery path is at one of its six places.
  /\ dpc \in [Owners -> [Paths -> {"idle", "decide", "cas", "callbacks", "hooks", "spent"}]]

\* AT MOST ONCE. No attempt's callbacks run twice or both ways, and the
\* hooks' outcome runs at most once per attempt. Lean: exactly_once.
\* Example ruled out: the queue's poll and the queue's drop both deliver the
\* same commit, and the shop's "order placed" message goes out twice. The
\* program needs this because a callback acts outside the database, where
\* nothing can be rolled back.
AtMostOnce ==
  \* For every owner and attempt:
  \A x \in Owners, a \in Attempts :
    \* its on_commit and its on_abort callbacks together ran at most once;
    /\ ranC[x][a] + ranA[x][a] <= 1
    \* and so did the hooks' outcome.
    /\ hookC[x][a] + hookA[x][a] <= 1

\* Nothing is in flight: every transact call returned and every worker is
\* idle.
AllSettled ==
  \* Every transact call returned.
  /\ \A x \in Owners : opc[x] = "done"
  \* Every committer is idle.
  /\ \A c \in Committers : cpc[c] = "idle"
  \* No delivery is under way.
  /\ \A x \in Owners, p \in Paths : dpc[x][p] \in {"idle", "spent"}
  \* close was never called, or is done.
  /\ clpc \in {"idle", "closed"}
  \* Nothing waits in the pipeline.
  /\ subq = <<>>

\* EXACTLY ONCE. Once everything has settled, every attempt that began ran
\* exactly one outcome: its on_commit or its on_abort callbacks, and the
\* hooks' matching one. Lean: exactly_once.
\* Example ruled out: close and the owner each leave the abort to the other,
\* neither runs on_abort, and the reserved stock is never released. The
\* program needs this because a caller that registered a callback is waiting
\* to be told how its transaction ended.
ExactlyOnce ==
  \* Once everything has settled,
  AllSettled =>
    \A x \in Owners, a \in Attempts :
      \* for every attempt that began:
      a <= att[x] =>
        \* exactly one of its on_commit and on_abort callbacks ran;
        /\ ranC[x][a] + ranA[x][a] = 1
        \* exactly one hook outcome ran;
        /\ hookC[x][a] + hookA[x][a] = 1
        \* and on_commit ran exactly when the attempt committed.
        /\ (ranC[x][a] = 1 <=> committed[x][a])

\* A transact call commits at most once, and only with its last attempt.
\* Example ruled out: attempt 1 commits, transact runs the closure again, and
\* the caller's change is applied twice.
OneCommitPerTransact ==
  \* A committed attempt is its owner's current one: after a commit, transact starts no further
  \* attempt.
  \A x \in Owners, a \in Attempts : committed[x][a] => a = att[x]

\* ORDER. The hooks' before_commit runs after the transaction's, validation
\* after both, and the hooks' outcome after the transaction's callbacks.
\* Lean: order.
\* Example ruled out: validation runs first and a before_commit callback then
\* adds a write nobody checked; or a hook that records "committed" runs before
\* the transaction's own on_commit. The program needs this because the order
\* is documented and callers build on it.
CallbacksBeforeHooks ==
  \* For every owner and attempt:
  \A x \in Owners, a \in Attempts :
    \* the hooks' before_commit ran only after the transaction's own;
    /\ hookBefore[x][a] => beforeRan[x][a]
    \* validation ran only after the hooks' before_commit;
    /\ validated[x][a] => hookBefore[x][a]
    \* the hooks' on_commit ran only after the transaction's on_commit;
    /\ hookC[x][a] > 0 => ranC[x][a] > 0
    \* the hooks' on_abort ran only after the transaction's on_abort.
    /\ hookA[x][a] > 0 => ranA[x][a] > 0

\* BEFORE_COMMIT WRITES ARE VALIDATED. Every committed attempt's callback
\* added one to the counter it read at its snapshot; with those writes
\* validated, no two commits read the same value, so the counter is
\* exactly the number of commits. Lean: before_writes_validated.
\* Example ruled out: two transactions both read 0 in their callbacks and
\* both commit 1. One increment is lost, because validation looked only at
\* the keys the closures wrote and never at the counter.
NoLostUpdate ==
  \* The counter equals the number of (owner, attempt) pairs that committed.
  ctr = Cardinality({p \in Owners \X Attempts : committed[p[1]][p[2]]})

\* ATTEMPTS ARE ISOLATED. Every callback that ran reported the outcome of
\* its own attempt: a re-run never runs a previous attempt's callbacks.
\* Lean: attempts_isolated.
\* Example ruled out: attempt 1 conflicts and attempt 2 commits; attempt 1's
\* on_commit, still registered, runs with attempt 2's outcome, so the caller
\* acts twice on one commit, once with values the failed attempt computed.
AttemptIsolation == \A r \in ranFor : r[2] = r[3]

\* ON THE COMMITTING THREAD (D53). Every outcome callback ran on the thread
\* that holds the commit's queue (its poll, or its drop), on the owner's
\* own thread (a blocking commit, a commit_nowait that was done when it
\* returned, or an abort the owner chose), or on the closing thread for a
\* transaction close aborted; never on a committer. Example ruled out: the
\* thread that ran a sync running another commit's on_commit on its own
\* thread while that commit's thread is busy. Lean: Callbacks.lean,
\* delivered_on_queue_thread.
OnCommittingThread == \A r \in runBy : r[2] # "committer"

\* on_commit runs only once the commit is durable, at Immediate.
\* Example ruled out: on_commit tells the customer the order is placed, the
\* machine loses power before the fsync, and after the restart the order is
\* gone. The program needs this because Immediate durability promises that a
\* reported commit survives a crash.
CommitAfterDurable ==
  \* At Immediate durability,
  Durability = "Immediate" =>
    \* an owner whose current attempt's on_commit ran has a commit that the log sync already covers.
    \A x \in Owners : att[x] > 0 /\ ranC[x][att[x]] > 0 => durable >= cseq[x]

\* RESOLVED AT DELIVERY, AFTER LANDING. A ticket's outcome is decided only
\* from a slot that holds it, of a commit that has landed: a commit owed an
\* fsync is not even known to its ticket before the fsync.
\* Example ruled out: the ticket of a commit at Immediate is decided when the
\* commit is written; a caller that blocks on the ticket's decision sees
\* "committed", the machine loses power before the fsync, and the commit is
\* gone.
ResolvedAfterLanding ==
  \* For every owner whose ticket has an outcome:
  \A x \in Owners : ps[x] \in {"decided", "delivering", "ready"} =>
    \* the pipeline decided it, and the commit has landed.
    gs[x] = "decided" /\ Landed(x)

\* A PANIC IN BEFORE_COMMIT ABORTS. The attempt never commits and never runs
\* on_commit. Lean: panic_aborts.
\* Example ruled out: a before_commit callback panics halfway through its
\* writes and the half-written transaction commits anyway.
PanicAborts ==
  \* For every owner and attempt:
  \A x \in Owners, a \in Attempts :
    \* one whose before_commit panicked did not commit, and its on_commit never ran.
    panicked[x][a] => ~committed[x][a] /\ ranC[x][a] = 0

----------------------------------------------------------------------------
\* Liveness.

\* Everything settles: every transact call returns, and every outcome has
\* run. Example ruled out: an outcome is decided but no path ever delivers
\* it, so its owner waits for its ticket for ever. Unlike the invariants,
\* which look at one state at a time, this is a promise about whole runs: TLC
\* checks that every run allowed by Fairness reaches a settled state.
Settles == <>AllSettled

====
