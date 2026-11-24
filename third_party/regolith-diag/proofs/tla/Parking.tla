---- MODULE Parking ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A caller that must not block (commit_nowait) leaves the work its commit
\* still needs as a job on its own queue and returns. Later the queue's owner
\* polls the queue, and the poll runs the job. The rule this model is about:
\*
\*     a poll never waits for another thread.
\*
\* Think of a shared workshop. A job needs two tools: first the round (the one
\* claim on the log's device step), then a second one (the version store's
\* step, the flush exclusion, or a failed log's writes in flight). Another
\* worker may be using a tool. A blocking caller that finds a tool in use
\* stands there until it is free: it chose to wait. A poll does not. It puts
\* the job back on the shelf, leaves a note on the tool ("tell me when you are
\* done"), puts back any tool it had already picked up, and goes on with the
\* rest of its queue. When the tool is put down, the note is read and every
\* queue that waits for the job is told it may go on.
\*
\* Tiny example. Queue A polls and claims the job. It takes the round, finds
\* the second tool in use, leaves its note there, gives the round back, puts
\* the job back (it is "free" again) and returns from the poll. The worker
\* with the second tool finishes and reads the note: queues A and B are both
\* told. B polls first, claims the job, takes both tools, runs it. A's poll
\* finds the job done. (When it is the round the worker has, A leaves its
\* note on the round and has nothing to give back.)
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - a poll stands waiting for a tool or for a job (NoPollWaits);
\*   - a poll parks a job and walks off with a tool (ParkedHoldsNothing), so
\*     everybody who needs that tool waits on a queue nobody may poll again;
\*   - the note is left a moment too late, after the tool was put down, and
\*     nobody ever reads it: the job sits on the shelf for ever (JobLands);
\*   - the queue that parked the job is never polled again, and the other
\*     queues that wait for the same job were never told (JobLands);
\*   - a blocking caller went to sleep on the job while the poll had it, the
\*     poll parked it, and nobody woke the caller (JobLands);
\*   - the tool is put down while the poll that will park the job still has
\*     it claimed (it left its note, looked again, and is giving the round
\*     back). The other queues are told "the job may go on" too early: they
\*     try the claim, lose, and forget the job; then the poll parks it and
\*     nobody tells them again (JobLands). So a wake that finds the job
\*     claimed is kept, and told when the claim is given up;
\*   - the job's body runs twice (RunsOnce).
\*
\* WHAT IS MODELLED, and the code each piece mirrors (src/engine):
\*   job, who    the job's claim word: free, claimed by one thread, done
\*               (io/flight.rs; io/job.rs Job::claim, Job::step, Job::run)
\*   tool        who holds each of the two tools. "a" is the round
\*               (commit/round.rs hold_round). "b" stands for the version
\*               store's step (step.rs Step::hold_or_park), the flush
\*               exclusion (flush.rs Flusher::hold_or_park) and a failed
\*               log's writes in flight (wal_gate.rs Gate::clear_or_park):
\*               all three are "take it or leave a waker and look again"
\*   notes       the wakers left on each tool (io/parked.rs Parked; the
\*               round's are Job::watch)
\*   ready       a queue's inbox holds "the job may go on" (Message::Ready)
\*   later       the queues whose wake came while the job was claimed: they
\*               are told when its holder lets go (Job::ready, Job::watch)
\*   listed      the job is in a queue's line of things to run
\*               (io_queue/queue.rs, the `order` list)
\*   sleepers    blocking callers registered to be woken by the job
\*               (Job::wait_turn), and asleep, the ones parked on it
\*   pc          where each thread is. A poller's steps are: claim, look at
\*               a tool, leave a note, look again, give back, park, tell. A
\*               blocker's are: claim or register, look again, sleep, wait
\*               for a tool, run.
\*   stopped     queue owners that will never poll again
\*   runs        how often the job's body ran
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Which job it is. A unit, a rotation check, the decision of a slot and
\*     a sync round all park the same way (commit/park.rs): every one of them
\*     has changed nothing when it parks, and starts over when it is stepped
\*     again. What each does once it has its tools is CommitPipeline.tla.
\*   - A job stepped inside another job's step. The inner one parks first and
\*     the outer one parks with it; the wake reaches both. That is this model
\*     twice over, and the loom models and the tests run it for real
\*     (src/engine/loom_model/park.rs, src/engine/commit/park_tests.rs).
\*   - Memory ordering. Each store and load here is one step, which is what
\*     the fences in the code make true; loom checks the fences.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one promise
\* it names):
\*   MC_Parking_Green                 two queues and one blocking caller, a
\*                                    worker with the second tool, either
\*                                    queue's owner may stop polling for
\*                                    good. Every promise holds.
\*   MC_Parking_Green_Round           the same with the worker holding the
\*                                    round.
\*   MC_Parking_Green_OneQueue        one queue and nobody else: the sizes
\*                                    of three of the RED rows, with the
\*                                    real code.
\*   MC_Parking_Green_Sleeper         one queue, whose owner may stop, and
\*                                    one blocking caller: the sizes of
\*                                    the last RED row, with the real code.
\*   MC_Parking_Green_Parks           the witness: the same run with the
\*                                    claim "no job is ever parked" added.
\*                                    It must break.
\*   MC_Parking_Red_PollWaits         a poll that waits for the tool as a
\*                                    blocking caller does: NoPollWaits.
\*   MC_Parking_Red_KeepsTool         a poll that parks without giving the
\*                                    round back: ParkedHoldsNothing.
\*   MC_Parking_Red_NoSecondLook      a poll that leaves its note and parks
\*                                    without looking at the tool again: the
\*                                    note may be left after the tool was put
\*                                    down, and JobLands breaks.
\*   MC_Parking_Red_ReleaseTellsNobody
\*                                    a tool put down without reading the
\*                                    notes: JobLands.
\*   MC_Parking_Red_WakeOnlyStepper   the wake tells only the queue that
\*                                    parked the job; that queue's owner
\*                                    stopped polling: JobLands.
\*   MC_Parking_Red_ParkTellsNobody   a park that does not wake the blocking
\*                                    caller asleep on the job: JobLands.
\*   MC_Parking_Red_WakeFindsClaimed  a wake that tells the queues at once
\*                                    though the job is still claimed:
\*                                    JobLands.
\*   MC_Parking_Red_StaleTellDelivers a poll that parked the job a while ago
\*                                    delivers the kept wakes though another
\*                                    poll has claimed the job since:
\*                                    JobLands.
\*
\* Lean, proofs/lean/Regolith/Parking.lean, proves the note and the second
\* look for any number of jobs and of hands a tool passes through: a job is
\* never parked beside a free tool with nobody left to tell it.

\* Numbers and the size of a set.
EXTENDS Naturals, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The queue owners: each polls its queue, which waits for the job.
  Pollers,
  \* The blocking callers that need the job done.
  Blockers,
  \* The thread at work with a tool when the run starts.
  Worker,
  \* The tool it holds: "a", the round, or "b", the second one.
  WorkerTool,
  \* The queue owners that may stop polling for good.
  CanStop,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The bug names this model knows.
ASSUME Mutant \in {"none", "PollWaits", "KeepsTool", "NoSecondLook",
                   "ReleaseTellsNobody", "WakeOnlyStepper", "ParkTellsNobody",
                   "WakeFindsClaimed", "StaleTellDelivers"}
\* Only queue owners stop polling.
ASSUME CanStop \subseteq Pollers
\* The worker holds one of the two tools.
ASSUME WorkerTool \in {"a", "b"}

\* Every thread of the run.
Actors == Pollers \cup Blockers \cup {Worker}
\* The two tools: the round first, then the second one.
Tools == {"a", "b"}
\* Nobody.
None == "nobody"

\* The state that changes from step to step.
VARIABLES
  \* "free", "claimed" or "done": the job's claim word.
  job,
  \* The thread that holds the claim, or None.
  who,
  \* [Tools -> Actors \cup {None}]: who holds each tool.
  tool,
  \* [Tools -> SUBSET Pollers]: the notes left on each tool.
  notes,
  \* [Pollers -> BOOLEAN]: the queue's inbox says the job may go on.
  ready,
  \* The queues whose note was read while the job was claimed.
  later,
  \* [Pollers -> BOOLEAN]: the job is in the queue's line of things to run.
  listed,
  \* The blocking callers registered to be woken by the job.
  sleepers,
  \* The blocking callers asleep on it.
  asleep,
  \* [Actors -> a step name]: where each thread is.
  pc,
  \* The queue owners that stopped polling.
  stopped,
  \* How often the job's body ran.
  runs,
  \* Whether a poll ever parked the job (for the witness).
  parkedOnce

\* All of them, for "nothing else changes".
vars == <<job, who, tool, notes, ready, later, listed, sleepers, asleep, pc, stopped, runs,
          parkedOnce>>

\* The start: the job waits on every queue, the worker has its tool.
Init ==
  \* Nobody claimed the job.
  /\ job = "free" /\ who = None
  \* The worker holds its tool and the other one is free.
  /\ tool = [t \in Tools |-> IF t = WorkerTool THEN Worker ELSE None]
  \* No note is left anywhere.
  /\ notes = [t \in Tools |-> {}]
  \* No queue was told anything yet, and no wake is kept for later.
  /\ ready = [q \in Pollers |-> FALSE] /\ later = {}
  \* The job is in every queue's line: each has a commit waiting on it.
  /\ listed = [q \in Pollers |-> TRUE]
  \* No blocking caller sleeps.
  /\ sleepers = {} /\ asleep = {}
  \* Every queue owner is between polls, every blocker at its start, and the
  \* worker is at work.
  /\ pc = [x \in Actors |-> IF x = Worker THEN "working" ELSE IF x \in Blockers THEN "start" ELSE "idle"]
  \* Nobody stopped, nothing ran, nothing was parked.
  /\ stopped = {} /\ runs = 0 /\ parkedOnce = FALSE

\* ---------------------------------------------------------------------------
\* Telling
\* ---------------------------------------------------------------------------

\* The queues the wake of the job parked by `q` tells: every queue that waits
\* for the job (Job::ready), or only `q` itself under the planted bug.
Told(q) == IF Mutant = "WakeOnlyStepper" THEN {q} ELSE Pollers

\* The inboxes after the notes `ns` were read: every queue told by one of them
\* now holds "the job may go on".
AfterWake(ns) == [p \in Pollers |-> ready[p] \/ (\E q \in ns : p \in Told(q))]

\* The inboxes after a tool was put down with the notes `ns` on it. Under the
\* planted bug ReleaseTellsNobody the notes are thrown away unread.
Released(ns) == IF Mutant = "ReleaseTellsNobody" THEN ready ELSE AfterWake(ns)

\* The notes `ns` are read while the job is claimed and the wake has to
\* wait for the claim to be given up (Job::ready finds the job not
\* runnable). The planted bug WakeFindsClaimed tells at once all the same.
Deferred(ns) ==
  \* There is a note, the job is claimed, and no planted bug tells at once.
  ns # {} /\ job = "claimed" /\ Mutant \notin {"WakeFindsClaimed", "ReleaseTellsNobody"}

\* Tool `t` is put down and the notes on it are read, in one step. In the
\* code these are two (the release, then the take of the notes), and the
\* job's note and second look are two more; the note and the look are two
\* steps here as well, which is where the race is.
PutDown(t) ==
  \* The tool is free.
  /\ tool' = [tool EXCEPT ![t] = None]
  \* Reading the notes clears them.
  /\ notes' = [notes EXCEPT ![t] = {}]
  \* Is the job in a poll's hands right now?
  /\ IF Deferred(notes[t])
       \* Yes: the queues the notes name are told when it lets go.
       THEN /\ later' = later \cup notes[t]
            \* Nobody is told yet.
            /\ UNCHANGED ready
       \* No: every queue a note names is told now.
       ELSE /\ ready' = Released(notes[t])
            \* No wake is kept.
            /\ UNCHANGED later

\* ---------------------------------------------------------------------------
\* A queue's poll (io_queue/queue.rs poll, commit/park.rs)
\* ---------------------------------------------------------------------------

\* The owner takes its inbox: a job it was told may go on joins its line.
Take(q) ==
  \* Between polls, still polling, and told.
  /\ pc[q] = "idle" /\ q \notin stopped /\ ready[q]
  \* The message is taken out of the inbox...
  /\ ready' = [ready EXCEPT ![q] = FALSE]
  \* ...and the job, if it is not done, is in the line again.
  /\ listed' = [listed EXCEPT ![q] = job # "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, tool, notes, later, sleepers, asleep, pc, stopped, runs, parkedOnce>>

\* The poll reaches the job in its line and tries to claim it with one
\* compare-and-swap.
Claim(q) ==
  \* Between polls, still polling, with the job in line.
  /\ pc[q] = "idle" /\ q \notin stopped /\ listed[q]
  \* Either way the job leaves the line: a job another thread has lands
  \* without this queue, and one that parks comes back with a message.
  /\ listed' = [listed EXCEPT ![q] = FALSE]
  \* The claim wins only on a free job.
  /\ IF job = "free"
       \* Won: this poll steps the job, starting with the round.
       THEN /\ job' = "claimed" /\ who' = q
            \* Its first need is the round.
            /\ pc' = [pc EXCEPT ![q] = "wantA"]
       \* Lost, or done already: the poll goes on with the rest of its queue.
       ELSE UNCHANGED <<job, who, pc>>
  \* Nothing else changes.
  /\ UNCHANGED <<tool, notes, ready, later, sleepers, asleep, stopped, runs, parkedOnce>>

\* The step needs tool `t` and looks at it for the first time.
Want(q, t, wantPc, nextPc, notePc) ==
  \* The poll is at this need.
  /\ pc[q] = wantPc
  \* A free tool is taken; a held one will get a note.
  /\ IF tool[t] = None
       \* Taken: on to the next need.
       THEN /\ tool' = [tool EXCEPT ![t] = q]
            \* The step goes on.
            /\ pc' = [pc EXCEPT ![q] = nextPc]
       \* Held by another thread. A poll never waits: it goes to leave its
       \* note. The planted bug PollWaits stands and waits instead.
       ELSE /\ UNCHANGED tool
            \* The note comes next, or the wait.
            /\ pc' = [pc EXCEPT ![q] = IF Mutant = "PollWaits" THEN "waits" ELSE notePc]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, notes, ready, later, listed, sleepers, asleep, stopped, runs, parkedOnce>>

\* The note: "tell me when this tool is put down". A step of its own, after
\* the first look: the tool may be put down in between, and then nobody reads
\* the note. That is what the second look is for.
Note(q, t, notePc, lookPc) ==
  \* The poll found the tool held.
  /\ pc[q] = notePc
  \* Its note is on the tool.
  /\ notes' = [notes EXCEPT ![t] = @ \cup {q}]
  \* The second look comes next.
  /\ pc' = [pc EXCEPT ![q] = lookPc]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, tool, ready, later, listed, sleepers, asleep, stopped, runs, parkedOnce>>

\* The second look, after the note is in place: the tool may have been put
\* down between the first look and the note, and then nobody reads the note.
Look(q, t, lookPc, nextPc, parkPc) ==
  \* The poll left its note on this tool.
  /\ pc[q] = lookPc
  \* A tool that is free now is taken after all. The planted bug NoSecondLook
  \* does not look and parks.
  /\ IF tool[t] = None /\ Mutant # "NoSecondLook"
       \* Taken: the note stays behind and wakes somebody for nothing later.
       THEN /\ tool' = [tool EXCEPT ![t] = q]
            \* The step goes on.
            /\ pc' = [pc EXCEPT ![q] = nextPc]
       \* Still held: the job parks.
       ELSE /\ UNCHANGED tool
            \* On to giving back what the step took.
            /\ pc' = [pc EXCEPT ![q] = parkPc]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, notes, ready, later, listed, sleepers, asleep, stopped, runs, parkedOnce>>

\* K1: before it parks, the step gives the round back, and whoever left a
\* note on the round is told. The planted bug KeepsTool keeps it.
GiveBack(q) ==
  \* The step could not get the second tool, and holds the round.
  /\ pc[q] = "giveA"
  \* The round is put down, or kept.
  /\ IF Mutant = "KeepsTool"
       \* Kept: nothing is told.
       THEN UNCHANGED <<tool, notes, ready, later>>
       \* Put down, notes read.
       ELSE PutDown("a")
  \* Then the job is put back.
  /\ pc' = [pc EXCEPT ![q] = "park"]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, listed, sleepers, asleep, stopped, runs, parkedOnce>>

\* The claim is given back without a landing (Flight::park): the job is free
\* again, with its body untouched.
Park(q) ==
  \* The step holds nothing it must give back.
  /\ pc[q] = "park"
  \* Free again, claimed by nobody.
  /\ job' = "free" /\ who' = None
  \* Then the callers that waited for this thread are told.
  /\ pc' = [pc EXCEPT ![q] = "tell"]
  \* A job was parked: the witness.
  /\ parkedOnce' = TRUE
  \* Nothing else changes.
  /\ UNCHANGED <<tool, notes, ready, later, listed, sleepers, asleep, stopped, runs>>

\* K4: the blocking callers registered on the job are woken, and look again
\* (Job::let_go). The planted bug ParkTellsNobody leaves them asleep. The
\* queues whose wake came while the job was claimed are told now, with the
\* job free to be claimed.
Tell(q) ==
  \* The job was just parked by this poll.
  /\ pc[q] = "tell"
  \* Did another thread claim the job since it was parked? The planted bug
  \* StaleTellDelivers does not look.
  /\ IF job = "claimed" /\ Mutant # "StaleTellDelivers"
       \* Yes: the kept wakes stay kept, for that thread's letting go (the
       \* wake looks at the claim again, Job::ready).
       THEN UNCHANGED <<ready, later>>
       \* No: the wakes kept for this moment are delivered ...
       ELSE /\ ready' = AfterWake(later)
            \* ... and none is kept any longer.
            /\ later' = {}
  \* The registered callers are taken off the list and woken, or not.
  /\ IF Mutant = "ParkTellsNobody"
       \* Nobody is told.
       THEN UNCHANGED <<sleepers, asleep>>
       \* Everybody registered is off the list and awake.
       ELSE /\ sleepers' = {}
            \* Awake: no longer asleep.
            /\ asleep' = asleep \ sleepers
  \* The poll returns to the rest of its queue.
  /\ pc' = [pc EXCEPT ![q] = "idle"]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, tool, notes, listed, stopped, runs, parkedOnce>>

\* ---------------------------------------------------------------------------
\* Running the job, by a poll or a blocking caller
\* ---------------------------------------------------------------------------

\* The body runs with both tools held, and the job lands: one step here, since
\* nothing another thread does can fall inside it (both tools are held).
Run(x) ==
  \* The thread holds the claim and both tools.
  /\ pc[x] = "run"
  \* The body ran.
  /\ runs' = runs + 1
  \* Landed: done for good, claimed by nobody.
  /\ job' = "done" /\ who' = None
  \* Both tools are put down.
  /\ tool' = [t \in Tools |-> None]
  \* Their notes are read...
  /\ notes' = [t \in Tools |-> {}]
  \* ...and the queues they name are told, to find the job done.
  /\ ready' = Released(notes["a"] \cup notes["b"] \cup later)
  \* No wake is kept: the landing tells everybody.
  /\ later' = {}
  \* The landing wakes every caller asleep on the job.
  /\ sleepers' = {} /\ asleep' = {}
  \* The thread is through.
  /\ pc' = [pc EXCEPT ![x] = IF x \in Pollers THEN "idle" ELSE "end"]
  \* Nothing else changes.
  /\ UNCHANGED <<listed, stopped, parkedOnce>>

\* ---------------------------------------------------------------------------
\* A blocking caller (commit/park.rs run_or_wait, io/job.rs wait_turn)
\* ---------------------------------------------------------------------------

\* The caller tries the claim; when another thread has the job it registers
\* to be woken.
BlockerClaim(b) ==
  \* At its start, or woken and looking again.
  /\ pc[b] = "start"
  \* What it finds decides what it does.
  /\ IF job = "free"
       \* It claims the job and runs it as a blocking caller.
       THEN /\ job' = "claimed" /\ who' = b
            \* It needs the round first.
            /\ pc' = [pc EXCEPT ![b] = "waitA"]
            \* It registers nowhere.
            /\ UNCHANGED sleepers
       \* Not free: landed already, or another thread has it.
       ELSE IF job = "done"
         \* Nothing left to do.
         THEN /\ pc' = [pc EXCEPT ![b] = "end"]
              \* Nothing else moves.
              /\ UNCHANGED <<job, who, sleepers>>
         \* Another thread has it: register, then look again.
         ELSE /\ sleepers' = sleepers \cup {b}
              \* The second look comes next.
              /\ pc' = [pc EXCEPT ![b] = "relook"]
              \* The job is untouched.
              /\ UNCHANGED <<job, who>>
  \* Nothing else changes.
  /\ UNCHANGED <<tool, notes, ready, later, listed, asleep, stopped, runs, parkedOnce>>

\* Registered, the caller looks again before it sleeps: the job may have
\* landed or been parked since its claim lost.
BlockerLook(b) ==
  \* Registered and about to sleep.
  /\ pc[b] = "relook"
  \* A job still claimed is slept on; anything else is looked at from the top.
  /\ IF job = "claimed"
       \* Asleep until the landing or the park wakes it.
       THEN /\ asleep' = asleep \cup {b}
            \* It sleeps.
            /\ pc' = [pc EXCEPT ![b] = "sleeps"]
       \* Landed or parked meanwhile: no sleep.
       ELSE /\ UNCHANGED asleep
            \* Back to the claim.
            /\ pc' = [pc EXCEPT ![b] = "start"]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, tool, notes, ready, later, listed, sleepers, stopped, runs, parkedOnce>>

\* A woken caller looks again from the top.
BlockerWakes(b) ==
  \* It slept, and somebody woke it.
  /\ pc[b] = "sleeps" /\ b \notin asleep
  \* Back to the claim.
  /\ pc' = [pc EXCEPT ![b] = "start"]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, tool, notes, ready, later, listed, sleepers, asleep, stopped, runs,
                 parkedOnce>>

\* A blocking caller waits for a tool another thread holds: it chose to
\* block. This is the stated limit of the design (the rustdoc of
\* src/engine/commit/mod.rs; CommitPipeline.tla, StopHoldsClaim).
BlockerTakes(b, t, waitPc, nextPc) ==
  \* It needs this tool, and the tool is free.
  /\ pc[b] = waitPc /\ tool[t] = None
  \* It takes it.
  /\ tool' = [tool EXCEPT ![t] = b]
  \* On to its next need, or to the body.
  /\ pc' = [pc EXCEPT ![b] = nextPc]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, notes, ready, later, listed, sleepers, asleep, stopped, runs, parkedOnce>>

\* ---------------------------------------------------------------------------
\* The worker with the second tool, and owners that stop
\* ---------------------------------------------------------------------------

\* The worker finishes and puts its tool down.
WorkerDone ==
  \* Still at work.
  /\ pc[Worker] = "working"
  \* The tool is put down and its notes are read.
  /\ PutDown(WorkerTool)
  \* The worker is through.
  /\ pc' = [pc EXCEPT ![Worker] = "end"]
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, listed, sleepers, asleep, stopped, runs, parkedOnce>>

\* A queue's owner stops polling for good, between two polls.
Stop(q) ==
  \* It may stop, has not yet, and is between polls.
  /\ q \in CanStop /\ q \notin stopped /\ pc[q] = "idle"
  \* At most one owner stops: somebody is left to poll.
  /\ stopped = {}
  \* It never polls again.
  /\ stopped' = stopped \cup {q}
  \* Nothing else changes.
  /\ UNCHANGED <<job, who, tool, notes, ready, later, listed, sleepers, asleep, pc, runs, parkedOnce>>

\* ---------------------------------------------------------------------------
\* The whole
\* ---------------------------------------------------------------------------

\* Every step a queue's poll takes.
PollStep(q) ==
  \* Take the inbox, or claim the job.
  \/ Take(q) \/ Claim(q)
  \* Need the round: taken goes on to the second tool, held leaves a note.
  \/ Want(q, "a", "wantA", "wantB", "noteA") \/ Note(q, "a", "noteA", "lookA")
  \* Look at the round again: taken goes on, held parks with nothing to give back.
  \/ Look(q, "a", "lookA", "wantB", "park")
  \* Need the second tool: taken goes on to the body, held leaves a note.
  \/ Want(q, "b", "wantB", "run", "noteB") \/ Note(q, "b", "noteB", "lookB")
  \* Look at it again: taken runs, held gives the round back first.
  \/ Look(q, "b", "lookB", "run", "giveA")
  \* Give the round back, park, tell.
  \/ GiveBack(q) \/ Park(q) \/ Tell(q)
  \* Run the body.
  \/ Run(q)

\* Every step a blocking caller takes.
BlockStep(b) ==
  \* Claim or register, look again, wake.
  \/ BlockerClaim(b) \/ BlockerLook(b) \/ BlockerWakes(b)
  \* Wait for the round, then for the second tool.
  \/ BlockerTakes(b, "a", "waitA", "waitB") \/ BlockerTakes(b, "b", "waitB", "run")
  \* Run the body.
  \/ Run(b)

\* One step of the run.
Next ==
  \* A poll's step.
  \/ \E q \in Pollers : PollStep(q)
  \* A blocking caller's.
  \/ \E b \in Blockers : BlockStep(b)
  \* The worker finishing.
  \/ WorkerDone
  \* An owner that stops polling.
  \/ \E q \in Pollers : Stop(q)

\* The run: the start, steps, and every thread that can go on does go on,
\* except that an owner may stop polling (Stop is not forced) and a stopped
\* owner takes no step at all (its steps are never enabled).
Spec ==
  \* The start and the steps.
  /\ Init /\ [][Next]_vars
  \* A poll that can go on does.
  /\ \A q \in Pollers : WF_vars(PollStep(q))
  \* A blocking caller that can go on does.
  /\ \A b \in Blockers : WF_vars(BlockStep(b))
  \* The worker finishes.
  /\ WF_vars(WorkerDone)

\* ---------------------------------------------------------------------------
\* What must be true
\* ---------------------------------------------------------------------------

\* Every variable holds the kind of thing its comment says.
TypeOK ==
  \* The claim word and its holder.
  /\ job \in {"free", "claimed", "done"} /\ who \in Actors \cup {None}
  \* Tools are held by a thread or by nobody.
  /\ tool \in [Tools -> Actors \cup {None}]
  \* Notes name queue owners.
  /\ notes \in [Tools -> SUBSET Pollers]
  \* The two flags of a queue.
  /\ ready \in [Pollers -> BOOLEAN] /\ listed \in [Pollers -> BOOLEAN]
  \* Sleepers are blocking callers, and a kept wake names queue owners.
  /\ sleepers \subseteq Blockers /\ asleep \subseteq Blockers /\ later \subseteq Pollers
  \* Stopped threads are queue owners, and the count is a number.
  /\ stopped \subseteq Pollers /\ runs \in Nat /\ parkedOnce \in BOOLEAN
  \* Every thread is at a step this model names.
  /\ pc \in [Actors -> {"idle", "wantA", "noteA", "lookA", "wantB", "noteB", "lookB", "giveA",
                         "park", "tell", "waits", "run", "start", "relook", "sleeps", "waitA",
                         "waitB", "working", "end"}]

\* THE RULE: a poll never waits for another thread. Its only way to be
\* waiting is the planted bug's.
NoPollWaits == \A q \in Pollers : pc[q] # "waits"

\* K1: a poll that is between polls holds no tool. So a job it parked can be
\* claimed and run by anybody, and an owner that never polls again holds
\* nobody up.
ParkedHoldsNothing == \A q \in Pollers : pc[q] = "idle" => \A t \in Tools : tool[t] # q

\* The claim: a claimed job has exactly one holder, a free or done one none.
OneClaim == (job = "claimed") <=> (who # None)

\* The body runs at most once.
RunsOnce == runs <= 1

\* The witness: no job is ever parked. False on purpose: the GREEN run with
\* this added must break, which shows it does go through a park.
NeverParked == ~parkedOnce

\* K2 and K4, as one promise about whole runs: the job lands. With a queue
\* owner that stops polling after it parked the job, with a blocking caller
\* that went to sleep while a poll had it, and with a note left just as the
\* tool was put down.
JobLands == <>(job = "done")

====
