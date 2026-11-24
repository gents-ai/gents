---- MODULE CompactionGate ----
\* THE STORY, FOR A READER WHO HAS NEVER SEEN THE CODE.
\*
\* A database keeps its data in files called tables. In the background a
\* worker thread keeps tidying them: it merges a few small tables into one
\* bigger table and then deletes the small ones. One round of tidying is
\* called a PASS. Several passes may run side by side, as long as each works
\* on its own tables.
\*
\* A few operations must not see a table deleted under them: copying every
\* table for a checkpoint or a backup, adding a table built elsewhere (an
\* ingest), compacting one key range by hand, dropping all the data. We
\* call each of these a JOB. While a job runs, no pass may run, and no other
\* job.
\*
\* The door that enforces this is the COMPACTION GATE. A pass goes through it
\* "shared": many passes fit inside together. A job goes through it "alone":
\* it is inside with nobody else. In the code the gate is
\* src/engine/compaction_gate.rs, a regolith::sync::RwLock.
\*
\* WHAT CAN GO WRONG (defect E28). Picture a shop with one counter. The old
\* gate had this rule: "while any job waits, no new pass may come in". Now
\* let jobs keep arriving, each one showing up while the one before is
\* still at the counter. There is always a job waiting, so the pass never
\* gets in. The worker stops tidying, the pile of small tables grows, and
\* the database stops accepting writes until the jobs happen to pause.
\*   The worker had a second way to starve itself: it ran EVERY job it had
\* been handed before doing any tidying of its own. With jobs handed over
\* faster than it ran them, it never tidied at all.
\*
\* THE RULES THE NEW CODE FOLLOWS, which this module writes down and checks.
\*   1. Whoever finds the gate free for them walks in, even past others who
\*      wait (this is called barging: nobody waits for a sleeping thread).
\*   2. A waiter keeps its place in a queue. Each time the gate frees, the
\*      waiter at the front is woken and tries again. If it loses again,
\*      because somebody barged in first, it counts one more loss.
\*   3. A waiter that has lost Bound times is OWED the gate: from that
\*      moment nobody barges, the threads inside finish and leave, and the
\*      gate is handed to the waiters in queue order until the owed one is
\*      in. So no pass and no job is passed over more than Bound times.
\*   4. The worker takes turns: one job, then one pass, then one job.
\*   5. A job that belongs to a caller's own I/O queue never makes that
\*      caller wait. If the gate is busy the job PARKS: it stays in the
\*      gate's queue, and when the gate wakes it, the wake is a note left
\*      in the caller's inbox. The caller runs the job the next time it
\*      looks at its inbox.
\*
\* A TINY EXAMPLE of rule 3 with Bound = 2. Job A is inside. The worker asks
\* for a pass, cannot enter, and queues. A leaves, and job B barges in before
\* the worker runs: loss 1. B leaves, job C barges in: loss 2, which is
\* Bound, so the worker is now owed. Job D arrives and may NOT barge: it
\* queues behind the worker. C leaves, and the gate is handed straight to
\* the worker's pass. D goes after it.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - How the lock's queue is built from atomics (the arrival stack, the
\*     drain role, the waker slot). SyncRwLock.tla and Sync.tla check those
\*     step by step for this same lock. Here one acquire attempt, one
\*     release and the wake-ups it decides are each one step.
\*   - The poll that queues a waiter does not count a loss it takes inside
\*     that same poll; the model counts it. So the model's waiter is owed
\*     no later than the code's, and the bound holds for the code too.
\*   - What a pass or a job does with the tables. Only who is inside the
\*     gate matters here.
\*   - The worker's flush, which runs between its job and its pass and
\*     never touches the gate.
\*   - A pass that a caller's own call runs (`Db::compact_step`, a step a
\*     write owes). It only tries the gate: it walks in, exactly as a
\*     worker's pass that barges does here, or it goes away having changed
\*     nothing. It never waits, so it is never a waiter to be fair to.
\*   - A waiter that gives up (a parked job settled by close, or by its
\*     queue being dropped). The lock's rule for it, "a waiter that leaves
\*     passes on what it was given", is checked in Sync.tla
\*     (CancelPassesOn). Here every waiter stays until it is in.
\*
\* PROVED FOR EVERY NUMBER OF THREADS in Lean,
\* proofs/lean/Regolith/CompactionGate.lean: mutual_exclusion,
\* bounded_bypass, owed_only_gets_nearer (once owed, every step serves the
\* waiter, brings it nearer, or is a newcomer queueing behind it),
\* worker_takes_turns and parked_job_keeps_its_wake, with a counterexample
\* for each defect below.
\*
\* CONFIGURATIONS (each MC_CompactionGate_*.cfg file says more).
\*   Green             one worker, two callers that wait on their own
\*                     thread, a stream of jobs handed to the worker.
\*   Green_All         every kind of thread at once: one worker, two
\*                     callers that wait, one caller whose job parks.
\*   Green_Queue       one worker, one caller with an I/O queue whose job
\*                     parks, one caller that waits on its own thread.
\*   Green_Passes      two workers whose passes overlap, one caller: the
\*                     fairness the other way round.
\*   Green_Live        the worker's pass and every waiting job are served
\*                     in the end, under fair scheduling.
\*   Red_OldGate       the old rule "a waiting job holds every pass back".
\*                     (Its file leaves out NoLostWake and
\*                     OwedShutsBarging: they speak of a queue's front and
\*                     of an owed waiter, and the old gate had neither.)
\*   Red_NoHandoff     nobody is ever owed the gate.
\*   Red_DrainJobs     the worker runs every job before any pass.
\*   Red_WakeDropped   the gate's wake for a parked job leaves no note.
\*   Red_StaleWaker    a parked job keeps a used-up wake-up handle.
\*   Red_SkipGate      a job walks in without asking the gate.

\* Naturals gives numbers and +, Sequences gives queues (Append, Head, Tail),
\* FiniteSets gives Cardinality, the size of a set.
EXTENDS Naturals, Sequences, FiniteSets

\* The values a configuration file fixes before a check starts.
CONSTANTS
  \* The compaction worker threads. Each takes turns: a job handed to the
  \* workers, then a pass. (compaction.rs, `compaction_loop`.)
  Workers,
  \* Caller threads that run a job themselves and wait on their own thread
  \* for the gate: `drop_all`, a backup, a job with no queue and no worker.
  \* (compaction_gate.rs, `CompactionGate::exclusive`.)
  Blockers,
  \* Caller threads with an I/O queue. Their job parks on the queue instead
  \* of waiting. (foreground.rs, `Foreground::step`; io/job.rs, Parking.)
  Owners,
  \* How many losses make a waiter owed the gate. MAX_BYPASS in the code
  \* (sync/contend.rs): 16 in a normal build, 2 under loom.
  Bound,
  \* The most jobs that can sit in the workers' job queue at once. The real
  \* queue has no such limit; a small one keeps the model finite while
  \* still letting a new job arrive every time the worker takes one.
  MaxJobs,
  \* "none" for the real design, or the name of one planted defect.
  Mutant

\* Every thread in the model.
Threads == Workers \cup Blockers \cup Owners

\* A value that is not a thread, meaning "nobody".
None == "nobody"

\* The three kinds of thread are different threads.
ASSUME Workers \cap Blockers = {} /\ Workers \cap Owners = {} /\ Blockers \cap Owners = {}
\* "nobody" is not the name of a thread.
ASSUME None \notin Threads
\* The bound and the queue limit are at least one.
ASSUME Bound \in Nat \ {0} /\ MaxJobs \in Nat \ {0}
\* Only the defects this module knows how to plant.
ASSUME Mutant \in {"none", "OldGate", "NoHandoff", "DrainJobs", "WakeDropped",
                   \* (the list goes on: two more defects)
                   "StaleWaker", "SkipGate"}

\* What changes from one step to the next.
VARIABLES
  \* The threads inside the gate "shared": passes in flight. In the code,
  \* the reader count in the lock's state word (sync/raw_rwlock.rs).
  shared,
  \* The threads inside the gate "alone": a job in flight. In the code, the
  \* writer bit of the same word. A set, so the model can show two threads
  \* inside at once if a defect lets them.
  alone,
  \* The threads waiting for the gate, oldest first. In the code, the lock's
  \* waiter list (sync/list.rs).
  q,
  \* What each thread is asking for or holding: "shared" or "alone".
  want,
  \* How many times each waiter has been woken, tried again and lost. In
  \* the code, `Contend::bypass` (sync/contend.rs).
  bypass,
  \* The waiter that is owed the gate, or None. In the code, the HANDOFF
  \* bit of the state word together with the mark on that waiter's node
  \* (sync/queue.rs).
  owedTo,
  \* TRUE when a wake-up has reached the thread and it has not acted on it
  \* yet. For a worker or a blocker: its thread was unparked
  \* (sync/internal.rs, `block_on`). For an owner: a `Message::Ready` note
  \* sits in its queue's inbox (io/shared.rs).
  wake,
  \* For an owner only: TRUE while the wake-up handle its job registered at
  \* its last step can still fire. Each handle fires once (io/job.rs,
  \* `Unpark::fired`).
  armed,
  \* TRUE when the gate was handed to a waiter that has not noticed yet: it
  \* is already inside, and learns so when it next runs.
  handed,
  \* Where each thread is: "out" (not asking), "queued" (waiting in q), or
  \* "in" (it knows it is inside the gate).
  pc,
  \* For a worker: whose turn is next, "job" or "pass".
  turn,
  \* How many jobs callers have handed to the workers and no worker has
  \* taken yet. In the code, `WorkerJobs::jobs` (compaction.rs).
  jobs,
  \* For a worker: how many jobs it has run since its last pass.
  sincePass,
  \* A note the model keeps for itself: TRUE if anybody ever barged in
  \* while a waiter was owed the gate. The code has no such variable.
  bargedOwed

\* Every variable, so that "nothing changes" can be said in one word.
vars == <<shared, alone, q, want, bypass, owedTo, wake, armed, handed, pc, turn,
          \* (the list goes on: the last three variables)
          jobs, sincePass, bargedOwed>>

----------------------------------------------------------------------------
\* THE GATE AS ONE VALUE. The pieces of the gate travel together in a record
\* G, so that "leave, then decide who to wake" can be written as one function
\* applied to the result of another.

\* The gate right now, as a record.
Gate == [shared |-> shared, alone |-> alone, q |-> q, want |-> want,
         \* (the record goes on: the counts, the owed waiter, the wake-ups,)
         bypass |-> bypass, owedTo |-> owedTo, wake |-> wake, armed |-> armed,
         \* (and the handoffs not yet noticed)
         handed |-> handed]

\* Make record G the gate of the next state.
Install(G) ==
  \* The passes inside become G's passes.
  /\ shared' = G.shared
  \* The job inside becomes G's job.
  /\ alone' = G.alone
  \* The queue becomes G's queue.
  /\ q' = G.q
  \* What each thread asks for or holds becomes what G says.
  /\ want' = G.want
  \* The loss counts become G's.
  /\ bypass' = G.bypass
  \* The owed waiter becomes G's.
  /\ owedTo' = G.owedTo
  \* The pending wake-ups become G's.
  /\ wake' = G.wake
  \* The owners' handles become G's.
  /\ armed' = G.armed
  \* The not-yet-noticed handoffs become G's.
  /\ handed' = G.handed

\* Would a request of kind k fit into gate G right now? A pass fits unless a
\* job is inside. A job fits only into an empty gate.
Fits(G, k) ==
  \* A pass: no job may be inside.
  IF k = "shared" THEN G.alone = {}
  \* A job: nobody at all may be inside.
  ELSE G.alone = {} /\ G.shared = {}

\* The old gate's rule for the same question (defect OldGate): a pass is
\* kept out not only by a job inside but by any job that is waiting.
FitsOld(G, k) ==
  \* A pass: no job inside, and no job anywhere in the queue.
  IF k = "shared" THEN G.alone = {} /\ \A i \in 1..Len(G.q) : G.want[G.q[i]] # "alone"
  \* A job: an empty gate, as before.
  ELSE G.alone = {} /\ G.shared = {}

\* Gate G with thread t inside, as a pass or as a job according to k.
Inside(G, t, k) ==
  \* A pass joins the passes inside.
  IF k = "shared" THEN [G EXCEPT !.shared = @ \cup {t}]
  \* A job becomes the job inside.
  ELSE [G EXCEPT !.alone = @ \cup {t}]

\* Queue s with thread t taken out, wherever it stood.
Without(s, t) == SelectSeq(s, LAMBDA x : x # t)

\* Deliver a wake-up to thread t in gate G.
\*   A worker or a blocker is a parked thread: unparking it always works.
\*   An owner is told through the handle its job registered at its last
\* step. The handle works once: it leaves one note in the owner's inbox and
\* is then used up. (Defect WakeDropped: the handle is used up but leaves
\* no note.)
Wake(G, t) ==
  \* A thread that waits on its own stack: it will run again.
  IF t \notin Owners THEN [G EXCEPT !.wake = [@ EXCEPT ![t] = TRUE]]
  \* An owner whose handle was already used: nothing more is delivered.
  ELSE IF ~G.armed[t] THEN G
  \* An owner whose handle is fresh: the handle is used up now, and the
  \* note lands in the inbox unless the defect drops it.
  ELSE [G EXCEPT !.armed = [@ EXCEPT ![t] = FALSE],
                 \* The note: there afterwards unless the defect drops it
                 \* (a note already there stays).
                 !.wake = [@ EXCEPT ![t] = (Mutant # "WakeDropped") \/ @]]

\* Hand the gate to the waiter at the front of G's queue: it is inside from
\* this instant, it leaves the queue, it is marked "handed" so that it knows
\* when it runs, it is woken, and if it was the owed one nobody is owed any
\* more.
Grant(G) ==
  \* The waiter at the front.
  LET h == Head(G.q)
      \* The gate with h inside and gone from the queue.
      G1 == [Inside(G, h, G.want[h]) EXCEPT !.q = Tail(@),
                                         \* h was handed the gate and has
                                         \* not noticed yet.
                                         !.handed = [@ EXCEPT ![h] = TRUE],
                                         \* If h was the owed waiter, the
                                         \* debt is paid.
                                         !.owedTo = IF @ = h THEN None ELSE @]
  \* Tell h, so that it runs and finds itself inside.
  IN Wake(G1, h)

\* While a waiter is owed, hand the gate out in queue order for as long as
\* the front waiter fits: a job at the front takes it alone and the handing
\* stops; passes at the front go in one after another.
RECURSIVE GrantFront(_)
\* (The line above tells TLA+ that the definition below refers to itself.)
GrantFront(G) ==
  \* Nobody waits: nothing to hand out.
  IF G.q = <<>> THEN G
  \* The front waiter does not fit yet (somebody is still inside): stop.
  ELSE IF ~Fits(G, G.want[Head(G.q)]) THEN G
  \* The front waiter is a job: it takes the gate alone, and that is all.
  ELSE IF G.want[Head(G.q)] = "alone" THEN Grant(G)
  \* The front waiter is a pass: let it in and look at the next one.
  ELSE GrantFront(Grant(G))

\* Wake every pass that waits directly behind position i of the queue, up to
\* the first job: passes that could share the gate with the front are woken
\* together.
RECURSIVE WakePassesFrom(_, _)
\* (The line above tells TLA+ that the definition below refers to itself.)
WakePassesFrom(G, i) ==
  \* Past the end of the queue: done.
  IF i > Len(G.q) THEN G
  \* A job stands here: the passes behind it wait for it.
  ELSE IF G.want[G.q[i]] = "alone" THEN G
  \* A pass stands here: wake it and go on to the next position.
  ELSE WakePassesFrom(Wake(G, G.q[i]), i + 1)

\* The ordinary wake-up, when nobody is owed: wake the front waiter if its
\* request fits now, or in any case if `front` says a release just happened
\* (it then competes and counts the loss if it loses). When the front is a
\* pass that fits, the passes right behind it are woken with it.
Nudge(G, front) ==
  \* Nobody waits: nothing to wake.
  IF G.q = <<>> THEN G
  \* Somebody waits: call the waiter at the front h.
  ELSE LET h == Head(G.q)
           \* Does the front waiter's request fit the gate as it is?
           fits == Fits(G, G.want[h])
           \* Wake the front if it fits or a release asked for it.
           G1 == IF fits \/ front THEN Wake(G, h) ELSE G
       \* A front pass that fits brings the passes behind it along.
       IN IF fits /\ G.want[h] = "shared" THEN WakePassesFrom(G1, 2) ELSE G1

\* What the lock does after anything that may let a waiter run (a release,
\* a new waiter, a waiter that took the gate). While a waiter is owed: hand
\* out in queue order, and if that served the owed waiter, go back to the
\* ordinary wake-up. Otherwise: the ordinary wake-up. In the code this is
\* one drain pass (sync/raw_rwlock.rs, `Policy::pass`).
Drain(G, front) ==
  \* Somebody is owed: hand the gate out in order.
  IF G.owedTo # None
    \* G1 is the gate after the handing out.
    THEN LET G1 == GrantFront(G)
         \* The owed waiter was served: ordinary wake-ups resume.
         IN IF G1.owedTo = None THEN Nudge(G1, front) ELSE G1
  \* Nobody is owed: the ordinary wake-up.
  ELSE Nudge(G, front)

\* The old gate's wake-up (defect OldGate): a release wakes everybody who
\* waits, and they all look again.
WakeAll(G) == [G EXCEPT !.wake = [t \in Threads |-> t \in {G.q[i] : i \in 1..Len(G.q)}]]

----------------------------------------------------------------------------
\* THE START. Nobody is inside, nobody waits, nothing is owed.

\* The first state: every part of the gate is empty, as listed below.
Init ==
  \* No pass is in flight.
  /\ shared = {}
  \* No job is in flight.
  /\ alone = {}
  \* Nobody waits for the gate.
  /\ q = <<>>
  \* Workers start by asking for nothing in particular; callers only ever
  \* ask to be alone.
  /\ want = [t \in Threads |-> "alone"]
  \* Nobody has lost anything yet.
  /\ bypass = [t \in Threads |-> 0]
  \* Nobody is owed the gate.
  /\ owedTo = None
  \* No wake-up is pending.
  /\ wake = [t \in Threads |-> FALSE]
  \* No owner has registered a handle yet.
  /\ armed = [t \in Threads |-> FALSE]
  \* The gate was handed to nobody.
  /\ handed = [t \in Threads |-> FALSE]
  \* Every thread is outside, asking for nothing.
  /\ pc = [t \in Threads |-> "out"]
  \* Every worker begins with a job turn.
  /\ turn = [w \in Workers |-> "job"]
  \* No job has been handed to the workers.
  /\ jobs = 0
  \* No worker has run a job yet.
  /\ sincePass = [w \in Workers |-> 0]
  \* Nobody has barged past an owed waiter.
  /\ bargedOwed = FALSE

----------------------------------------------------------------------------
\* ASKING FOR THE GATE.

\* Thread t asks for the gate as kind k, for the first time for this request.
\* If the request fits and nobody is owed, t is inside at once, even when
\* others wait: that is barging. Otherwise t joins the back of the queue.
\* In the code: the first poll of `RwLock::read` or `write_owned`
\* (sync/contend.rs, `Contend::poll`).
Acquire(t, k) ==
  \* Does the request fit? (The old gate asks its own question.)
  LET fits == IF Mutant = "OldGate" THEN FitsOld(Gate, k) ELSE Fits(Gate, k)
      \* May t walk in now? The old gate has no owed waiter to respect.
      \* (Defect SkipGate: a caller's job walks in without asking.)
      open == \/ fits /\ (owedTo = None \/ Mutant = "OldGate")
              \* The defect's way in: a caller's job, whatever the gate holds.
              \/ Mutant = "SkipGate" /\ t \in Blockers
  \* Two cases: t may walk in, or it may not.
  IN IF open
       \* t is inside: put it among the passes or make it the job, and
       \* remember which, so that leaving takes out the right thing.
       THEN /\ Install([Inside(Gate, t, k) EXCEPT !.want = [@ EXCEPT ![t] = k]])
            \* t knows it is inside.
            /\ pc' = [pc EXCEPT ![t] = "in"]
            \* Note it if this was a barge past an owed waiter: it must never be.
            /\ bargedOwed' = (bargedOwed \/ (owedTo # None /\ Mutant # "OldGate"))
       \* t cannot enter: it goes to the back of the queue, asking for k,
       \* with a clean count, no wake pending and, for an owner, a fresh
       \* handle.
       ELSE /\ LET G1 == [Gate EXCEPT !.q = Append(@, t),
                                      \* What t waits for.
                                      !.want = [@ EXCEPT ![t] = k],
                                      \* No loss counted yet.
                                      !.bypass = [@ EXCEPT ![t] = 0],
                                      \* No wake-up pending for it.
                                      !.wake = [@ EXCEPT ![t] = FALSE],
                                      \* The gate was not handed to it.
                                      !.handed = [@ EXCEPT ![t] = FALSE],
                                      \* An owner's job registers a handle
                                      \* as it parks; others have none.
                                      !.armed = [@ EXCEPT ![t] = t \in Owners]]
               \* A new waiter makes the lock look at its queue once (the
               \* old gate does nothing here: its waiters wait for a release).
               IN Install(IF Mutant = "OldGate" THEN G1 ELSE Drain(G1, FALSE))
            \* t now waits. If the drain just handed it the gate, it finds
            \* out when it next runs.
            /\ pc' = [pc EXCEPT ![t] = "queued"]
            \* Queueing is not barging.
            /\ UNCHANGED bargedOwed

\* A caller hands the workers one more job. Callers may do this at any time,
\* as fast as they like: this is the endless stream of E28. In the code,
\* `CompactionScheduler::submit` (compaction.rs).
Submit ==
  \* There is room in the model's bounded job queue.
  /\ jobs < MaxJobs
  \* One more job waits for a worker.
  /\ jobs' = jobs + 1
  \* Handing a job over touches neither the gate nor any thread.
  /\ UNCHANGED <<shared, alone, q, want, bypass, owedTo, wake, armed, handed, pc,
                 \* (the list goes on)
                 turn, sincePass, bargedOwed>>

\* Worker w, with nothing in hand, starts its next turn.
\*   A job turn: if a job waits, w takes it and asks for the gate alone. If
\* none waits, the turn passes to the pass with nothing done.
\*   A pass turn: w asks for the gate shared.
\* In the code, one round of `compaction_loop` (compaction.rs): `run_one`,
\* then the pass under `gate.shared()`.
WorkerAsks(w) ==
  \* w is not inside and not waiting.
  /\ pc[w] = "out"
  \* Which turn is it?
  /\ IF turn[w] = "job"
       \* A job turn: is a job waiting?
       THEN IF jobs > 0
              \* Yes: one job fewer waits for a worker,
              THEN /\ jobs' = jobs - 1
                   \* w asks for the gate alone, to run it,
                   /\ Acquire(w, "alone")
                   \* and the turn moves on only when the job is done (Leave).
                   /\ UNCHANGED <<turn, sincePass>>
              \* A job turn with no job: straight on to the pass turn.
              ELSE /\ turn' = [turn EXCEPT ![w] = "pass"]
                   \* Nothing else changes: w asked the gate for nothing.
                   /\ UNCHANGED <<shared, alone, q, want, bypass, owedTo, wake, armed,
                                  \* (the list goes on)
                                  handed, pc, jobs, sincePass, bargedOwed>>
       \* A pass turn: ask for the gate shared.
       ELSE /\ Acquire(w, "shared")
            \* The turn moves on only when the pass is done (Leave).
            /\ UNCHANGED <<turn, jobs, sincePass>>

\* Caller c, with nothing in hand, starts a job of its own and asks for the
\* gate alone. A blocker will wait on its own thread; an owner's job will
\* park. In the code: `CompactionGate::exclusive` for a blocker, the first
\* `Admission::poll` in `Foreground::step` for an owner.
CallerAsks(c) ==
  \* c is not inside and not waiting.
  /\ pc[c] = "out"
  \* Ask to be alone.
  /\ Acquire(c, "alone")
  \* A caller's job is not one of the jobs handed to the workers.
  /\ UNCHANGED <<turn, jobs, sincePass>>

----------------------------------------------------------------------------
\* WAITING.

\* A waiter that was woken tries again. If the gate fits it and nobody is
\* owed, it takes the gate and leaves the queue. If it loses, it stays where
\* it is in the queue and counts the loss; the loss that brings its count to
\* Bound makes it the owed waiter, in the same step, so that nothing can
\* slip in between. In the code: `Contend::poll` on `Park::Nudged`, and
\* `owe_or_take` (sync/contend.rs, sync/queue.rs).
\*   For a worker or a blocker this is its thread running again after an
\* unpark. For an owner it is the next poll of its queue: the owner takes
\* the note from its inbox and steps the job, which registers a fresh
\* handle (defect StaleWaker: it keeps the used-up one).
Retry(t) ==
  \* t waits in the queue,
  /\ pc[t] = "queued"
  \* a wake-up reached it,
  /\ wake[t]
  \* and the gate was not simply handed to it (that case is Collect).
  /\ ~handed[t]
  \* The gate with t's wake-up consumed and, for an owner, a fresh handle.
  /\ LET G0 == [Gate EXCEPT !.wake = [@ EXCEPT ![t] = FALSE],
                            \* An owner's job registers a fresh handle at
                            \* this step, unless the defect keeps the old one.
                            !.armed = [@ EXCEPT ![t] = (t \in Owners /\ Mutant # "StaleWaker")]]
         \* Does t's request fit? (The old gate asks its own question, and
         \* t itself, being a waiter, is not what holds a pass back.)
         fits == IF Mutant = "OldGate"
                   \* The old question, asked of the queue without t.
                   THEN FitsOld([G0 EXCEPT !.q = Without(@, t)], want[t])
                   \* The real question.
                   ELSE Fits(G0, want[t])
         \* Is somebody owed the gate? The old gate and the NoHandoff defect
         \* never owe it to anybody.
         owed == owedTo # None /\ Mutant # "OldGate"
         \* Would losing now be the loss that makes t owed?
         last == bypass[t] + 1 = Bound /\ Mutant \notin {"OldGate", "NoHandoff"}
         \* One more loss, kept from growing without end under the defects
         \* that never stop it (one past Bound is enough to show the fault).
         more == IF bypass[t] > Bound THEN bypass[t] ELSE bypass[t] + 1
     \* Four cases: t wins, somebody else is owed, t becomes owed, t loses.
     IN IF fits /\ ~owed
          \* t wins: it is inside and out of the queue, and the lock looks
          \* at the queue once more for whoever stands at the front now.
          THEN /\ LET G1 == [Inside(G0, t, want[t]) EXCEPT !.q = Without(@, t)]
                  \* (The old gate does no such looking.)
                  IN Install(IF Mutant = "OldGate" THEN G1 ELSE Drain(G1, FALSE))
               \* t knows it is inside.
               /\ pc' = [pc EXCEPT ![t] = "in"]
          \* Somebody else is owed: t stays queued and the loss is not
          \* counted, because it is the handoff, not a barger, that is ahead.
          ELSE IF owed
            \* Only t's wake-up was used up.
            THEN /\ Install(G0)
                 \* t still waits.
                 /\ UNCHANGED pc
          \* t loses for the Bound-th time: it is owed from this instant.
          \* The lock looks at its queue at once, in case what t waits for
          \* was freed just now.
          ELSE IF last
            \* Count the loss,
            THEN /\ Install(Drain([G0 EXCEPT !.bypass = [@ EXCEPT ![t] = more],
                                             \* and make t the owed waiter.
                                             !.owedTo = t], FALSE))
                 \* t still waits (if the drain handed it the gate, it finds
                 \* out when it next runs).
                 /\ UNCHANGED pc
          \* t loses: one more counted loss, and it waits for the next wake.
          ELSE /\ Install([G0 EXCEPT !.bypass = [@ EXCEPT ![t] = more]])
               \* t still waits.
               /\ UNCHANGED pc
  \* Trying again changes no turn, hands over no job and is not a barge.
  /\ UNCHANGED <<turn, jobs, sincePass, bargedOwed>>

\* A waiter to which the gate was handed runs and finds itself inside. In
\* the code: `Park::Granted` in `Contend::poll`.
Collect(t) ==
  \* t still thinks it waits,
  /\ pc[t] = "queued"
  \* but the gate was handed to it,
  /\ handed[t]
  \* and the wake-up that says so reached it.
  /\ wake[t]
  \* t knows it is inside now.
  /\ pc' = [pc EXCEPT ![t] = "in"]
  \* The handoff has been noticed.
  /\ handed' = [handed EXCEPT ![t] = FALSE]
  \* The wake-up has been used.
  /\ wake' = [wake EXCEPT ![t] = FALSE]
  \* Noticing changes nothing about who is inside or who waits.
  /\ UNCHANGED <<shared, alone, q, want, bypass, owedTo, armed, turn, jobs,
                 \* (the list goes on)
                 sincePass, bargedOwed>>

----------------------------------------------------------------------------
\* LEAVING.

\* Thread t, inside, finishes its pass or its job and leaves the gate. If
\* anybody waits, the lock looks at its queue: it hands the gate over when a
\* waiter is owed, and otherwise wakes the front waiter to try again. In the
\* code: dropping the guard (`read_unlock`, `write_unlock` in
\* sync/raw_rwlock.rs) and the drain pass a release asks for.
\*   A worker also moves on to its next turn: after a job, the pass (defect
\* DrainJobs: another job, as long as one waits); after a pass, a job.
Leave(t) ==
  \* t is inside and knows it.
  /\ pc[t] = "in"
  \* The gate without t.
  /\ LET G1 == [Gate EXCEPT !.shared = @ \ {t}, !.alone = @ \ {t}]
     \* Nobody waits: nothing more to do. The old gate wakes every waiter;
     \* the real one runs its drain with "a release just happened".
     IN Install(IF G1.q = <<>> THEN G1
                \* The old gate: everybody who waits looks again.
                ELSE IF Mutant = "OldGate" THEN WakeAll(G1)
                \* The real gate: one drain, told that a release happened.
                ELSE Drain(G1, TRUE))
  \* t is outside again.
  /\ pc' = [pc EXCEPT ![t] = "out"]
  \* A worker moves on to its next turn; a caller has none.
  /\ IF t \in Workers
       \* Did the worker just finish a job, or a pass?
       THEN IF want[t] = "alone"
              \* A job: one more job since its last pass (the count stops
              \* at 2, which is already one too many).
              THEN /\ sincePass' = [sincePass EXCEPT ![t] = IF @ >= 2 THEN 2 ELSE @ + 1]
                   \* The pass is next, unless the defect keeps the worker
                   \* on jobs while one waits.
                   /\ turn' = [turn EXCEPT ![t] = IF Mutant = "DrainJobs" /\ jobs > 0
                                                    \* (the defect's choice, then the real one)
                                                    THEN "job" ELSE "pass"]
              \* A pass: the count starts over,
              ELSE /\ sincePass' = [sincePass EXCEPT ![t] = 0]
                   \* and a job is next.
                   /\ turn' = [turn EXCEPT ![t] = "job"]
       \* A caller has no turns.
       ELSE UNCHANGED <<turn, sincePass>>
  \* Leaving hands over no job and is not a barge.
  /\ UNCHANGED <<jobs, bargedOwed>>

----------------------------------------------------------------------------
\* EVERYTHING THAT CAN HAPPEN.

\* One step of one thread.
Step(t) ==
  \* A worker starts its next turn,
  \/ t \in Workers /\ WorkerAsks(t)
  \* or a caller starts a job of its own,
  \/ t \in Blockers \cup Owners /\ CallerAsks(t)
  \* or a woken waiter tries again,
  \/ Retry(t)
  \* or a waiter finds the gate was handed to it,
  \/ Collect(t)
  \* or a thread inside leaves.
  \/ Leave(t)

\* One step of the whole system: a job is handed to the workers, or some
\* thread takes a step.
Next == Submit \/ \E t \in Threads : Step(t)

\* Fair scheduling: a thread that could take a step forever does take one.
\* This is what "the operating system runs every thread sooner or later,
\* and an owner keeps polling its queue" means. Handing jobs over is NOT
\* forced: callers may stop, or never stop.
Fairness == \A t \in Threads : WF_vars(Step(t))

\* The system: it starts at Init, moves by Next (or stands still), fairly.
Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
\* WHAT MUST ALWAYS BE TRUE.

\* Every variable holds the kind of value its description promises. This
\* catches mistakes in the model itself.
TypeOK ==
  \* Only threads are inside as passes.
  /\ shared \subseteq Threads
  \* Only threads are inside alone.
  /\ alone \subseteq Threads
  \* The queue holds threads.
  /\ q \in Seq(Threads)
  \* Every thread asks for one of the two kinds.
  /\ want \in [Threads -> {"shared", "alone"}]
  \* Loss counts are numbers.
  /\ bypass \in [Threads -> Nat]
  \* The owed waiter is a thread or nobody.
  /\ owedTo \in Threads \cup {None}
  \* A wake-up is pending or it is not.
  /\ wake \in [Threads -> BOOLEAN]
  \* A handle is fresh or it is not.
  /\ armed \in [Threads -> BOOLEAN]
  \* The gate was handed over or it was not.
  /\ handed \in [Threads -> BOOLEAN]
  \* Every thread is in one of the three places.
  /\ pc \in [Threads -> {"out", "queued", "in"}]
  \* Every worker's next turn is a job or a pass.
  /\ turn \in [Workers -> {"job", "pass"}]
  \* The job queue holds between none and MaxJobs jobs.
  /\ jobs \in 0..MaxJobs
  \* Every worker's count of jobs since its last pass is 0, 1 or 2.
  /\ sincePass \in [Workers -> 0..2]
  \* The model's own note is yes or no.
  /\ bargedOwed \in BOOLEAN

\* MUTUAL EXCLUSION. A job is inside with nobody else: no second job and no
\* pass. It rules out, for example, a checkpoint copying a table while a
\* pass deletes it. The database needs it because a job reads the list of
\* tables once and trusts it until it is done.
MutualExclusion ==
  \* At most one job is inside,
  /\ Cardinality(alone) <= 1
  \* and when a job is inside, no pass is.
  /\ (alone # {} => shared = {})

\* BOUNDED BYPASS, both ways (this is E28). No waiter, pass or job, counts
\* more than Bound losses: once it has lost Bound times it is owed the gate
\* and nobody gets in ahead of it. It rules out the worker's pass being
\* passed over again and again by a stream of jobs, and equally a job being
\* passed over again and again by passes that keep overlapping.
BoundedBypass == \A t \in Threads : bypass[t] <= Bound

\* AN OWED WAITER SHUTS BARGING. Nobody ever walks in past a waiter that is
\* owed the gate. Without this the bound above would mean nothing: the owed
\* waiter could go on waiting while others kept entering.
OwedShutsBarging == ~bargedOwed

\* THE WORKER TAKES TURNS (the other half of E28). No worker runs two jobs
\* without a pass in between. It rules out a worker that is handed jobs
\* faster than it runs them and so never tidies the tables again.
WorkerTakesTurns == \A w \in Workers : sincePass[w] <= 1

\* NO WAKE-UP IS LOST. Two cases cover every waiter that could move:
\*   - a waiter the gate was handed to has a wake-up pending, so it will run
\*     and find itself inside;
\*   - when the gate stands empty, the waiter at the front has a wake-up
\*     pending, so it will run and take it.
\* For an owner "a wake-up pending" is the note in its inbox: this is the
\* rule that a parked job is always pushed back to its queue. It rules out a
\* job parked for good beside a free gate, and a gate held for good by a
\* waiter that never learns it holds it.
NoLostWake ==
  \* A handed waiter has its wake-up.
  /\ \A t \in Threads : (pc[t] = "queued" /\ handed[t]) => wake[t]
  \* With the gate empty, the front waiter has its wake-up.
  /\ (q # <<>> /\ shared = {} /\ alone = {}) => wake[Head(q)]

\* THE BOOKS BALANCE. A thread that believes it is inside really is, as the
\* kind it asked for; and nobody is inside except threads that asked and
\* either know it or were just handed the gate. This catches a model (or a
\* design) in which a release takes out the wrong thread.
InsideMatches ==
  \* For every thread, both of these:
  \A t \in Threads :
    \* A thread that knows it is inside is among the passes or is the job.
    /\ (pc[t] = "in" => IF want[t] = "shared" THEN t \in shared ELSE t \in alone)
    \* A thread inside either knows it, or is a waiter the gate was handed to.
    /\ (t \in shared \cup alone => pc[t] = "in" \/ (pc[t] = "queued" /\ handed[t]))

----------------------------------------------------------------------------
\* WHAT MUST HAPPEN IN THE END (checked by Green_Live and Green_LiveServed,
\* under Fairness).

\* E28 as a promise about the future: every worker's pass gets its turn
\* again and again, however the jobs keep coming.
WorkerAlwaysPasses == \A w \in Workers : []<>(w \in shared)

\* Every thread that waits for the gate gets in.
EveryWaiterServed == \A t \in Threads : (pc[t] = "queued") ~> (pc[t] = "in")

====
