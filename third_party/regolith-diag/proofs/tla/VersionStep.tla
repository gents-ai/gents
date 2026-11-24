---- MODULE VersionStep ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* The version set says which tables the database is made of. Every change
\* to it (a flush installs a table, a compaction swaps tables, an ingest adds
\* one) reads the current version, builds the next one from it and installs
\* that. Two changes must never overlap: the second would build on a version
\* the first is about to replace, and the first one's table would be gone.
\*
\* A mutex used to keep them apart. The step replaces it
\* (src/engine/step.rs, src/engine/version_store.rs). Think of a workshop
\* with one workbench and a tray. A worker with a job for the bench puts it
\* on the tray. If nobody is at the bench, the worker who put the job down
\* is at the bench from that moment, and does every job on the tray, oldest
\* first, its own and other workers', until the tray is empty; then it
\* leaves. If somebody is at the bench, the worker just waits until its job
\* has been done, by whoever is at the bench. Nobody ever queues for the
\* bench itself: a job is never waiting for its own worker to be scheduled.
\*
\* One kind of worker may not wait at all: a job that a queue's poll runs
\* (src/engine/commit/park.rs). Before it starts work that puts jobs on the
\* tray it takes the bench for the whole of that work ("a hold"), if the
\* bench is free. While it has the bench, every job put on the tray is done
\* by it: at once when it puts one down itself, and when it leaves. If the
\* bench is taken, it does not wait: it leaves a note ("tell me when the
\* bench is free"), looks once more, and goes away; whoever leaves the bench
\* next reads the notes.
\*
\* Tiny example of the bug the step exists to prevent. The version is {}.
\* A flush reads it and builds {flush's table}. An ingest, let onto the bench
\* at the same time, reads {} too and builds {ingest's table}. The flush
\* installs its version, then the ingest installs its own over it: the
\* flush's table is in no version, and every row in it is lost.
\*
\* What this model checks cannot happen:
\*   - two sections run at once (OneAtATime, ST1), and so an edit that
\*     finished is missing from the version (NoLostEdit);
\*   - the bench is left with a job on the tray and nobody to do it
\*     (NoneLeftBehind, ST2);
\*   - sections run out of the order they were handed over in (InPushOrder,
\*     ST3);
\*   - a caller returns before its section has run (FinishedBeforeReturn,
\*     ST4), which would let the section use the caller's stack after the
\*     caller left it;
\*   - a job that keeps the step waits for another thread (NoHolderWaits,
\*     ST6);
\*   - a section never runs, or a job that parked for the step is never told
\*     (EverySectionRuns).
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   list     the sections handed over and not taken yet, oldest first
\*            (Step::waiting, a lock-free list; one push is one step)
\*   idle     the list's head word says nobody holds the step (IDLE). The
\*            push that finds it set clears it and holds the step from then
\*            on; so does a hold. One compare-and-swap either way.
\*   keeper   the thread that keeps a hold (Step::holder), or nobody
\*   batch    the sections a thread took off the list and has not run yet
\*            (Step::drain's `take`)
\*   running  the section a thread is in the middle of
\*   seen     the version that section read when it began
\*   version  the edits the version set holds
\*   pushed   a ghost: every section, in the order handed over
\*   ran      a ghost: every section, in the order finished
\*   done     the sections whose job has landed (Request::done)
\*   notes    the parked jobs' wakers (io/parked.rs, Parked)
\*   woken    the jobs told the step was given up
\*   pc       where each thread is
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - What an edit does. An edit is its own name, added to the version it
\*     read; that is enough for one edit to overwrite another.
\*   - A panic in a section (ST5): it is caught on the running thread and
\*     resumed on the caller's, which changes none of the steps here.
\*   - The publication of the version in the read view, which is the last
\*     thing a section does: ReadViewPublishers.tla.
\*   - More than one section a thread. A second section of the same thread
\*     is this model's steps again.
\*
\* A STATED LIMIT, which the last RED row shows rather than hides. A job
\* that keeps the step keeps it for the whole of its work (in the code: an
\* ordered unit a queue's poll runs holds the step, and the flush exclusion
\* when it flushes, for its body). A caller that hands a section over
\* meanwhile waits for that work to end, and a job that would keep the step
\* parks. So a thread that is not scheduled while it keeps the step holds
\* up every blocking caller behind it until it runs again. It is the same
\* limit as a blocking caller behind a claimed round (CommitPipeline.tla,
\* StopHoldsClaim).
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one promise
\* it names):
\*   MC_VersionStep_Green              two callers and one job that keeps
\*                                     the step. Every promise holds.
\*   MC_VersionStep_Green_Parks        the witness: the same run with the
\*                                     claim "no job ever parks" added. It
\*                                     must break.
\*   MC_VersionStep_Red_HoldWithoutCas a hold taken without looking whether
\*                                     the step is held: two sections run at
\*                                     once, OneAtATime breaks.
\*   MC_VersionStep_Red_HoldWithoutCas_LostEdit
\*                                     the same bug, its consequence: an
\*                                     edit is overwritten, NoLostEdit
\*                                     breaks.
\*   MC_VersionStep_Red_GiveUpBlind    the step given up without looking at
\*                                     the list again: a section pushed in
\*                                     between is left, NoneLeftBehind
\*                                     breaks.
\*   MC_VersionStep_Red_NewestFirst    the list taken newest first:
\*                                     InPushOrder breaks.
\*   MC_VersionStep_Red_ReturnEarly    a caller that does not wait for its
\*                                     section: FinishedBeforeReturn breaks.
\*   MC_VersionStep_Red_HolderWaits    a job that keeps the step hands a
\*                                     section over and waits for it as any
\*                                     caller does, though nobody but itself
\*                                     can run it: NoHolderWaits breaks.
\*   MC_VersionStep_Red_ReleaseTellsNobody
\*                                     the step given up without reading the
\*                                     notes: a parked job is never told,
\*                                     EverySectionRuns breaks.
\*   MC_VersionStep_Red_NoSecondTry    a job that leaves its note and parks
\*                                     without trying again: the step may
\*                                     have been given up in between, and
\*                                     EverySectionRuns breaks.
\*   MC_VersionStep_Red_KeeperStops    the stated limit: the real code, with
\*                                     a job that stops for good while it
\*                                     keeps the step. EverySectionRuns
\*                                     breaks.
\*
\* Lean, proofs/lean/Regolith/VersionStep.lean, proves the order, the
\* exclusion and that nothing is left behind for any number of sections.

\* Numbers, sequences and the size of a set.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The threads that hand one section over and wait for it (Step::run).
  Callers,
  \* The jobs that keep the step while they hand one section over
  \* (Step::hold_or_park): they never wait.
  Holders,
  \* The jobs that may stop for good while they keep the step.
  MayStop,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The bug names this model knows.
ASSUME Mutant \in {"none", "HoldWithoutCas", "GiveUpBlind", "NewestFirst", "ReturnEarly",
                   "HolderWaits", "ReleaseTellsNobody", "NoSecondTry"}

\* Only a job that keeps the step can stop while keeping it.
ASSUME MayStop \subseteq Holders

\* Every thread. Each has one section, named after it.
Threads == Callers \cup Holders

\* Nobody, and no section.
None == "nobody"

\* `s` back to front.
Reverse(s) == [i \in 1..Len(s) |-> s[Len(s) - i + 1]]

\* The state that changes from step to step.
VARIABLES
  \* The sections handed over and not taken yet, oldest first.
  list,
  \* The head word's flag: nobody holds the step.
  idle,
  \* The thread that keeps a hold, or None.
  keeper,
  \* [Threads -> a sequence of sections]: taken off the list, not run yet.
  batch,
  \* [Threads -> a section or None]: the section the thread is in.
  running,
  \* [Threads -> a set of edits]: the version that section read.
  seen,
  \* The edits the version set holds.
  version,
  \* Ghost: every section, in the order handed over.
  pushed,
  \* Ghost: every section, in the order finished.
  ran,
  \* The sections whose job has landed.
  done,
  \* The parked jobs whose waker is on the step.
  notes,
  \* The jobs told that the step was given up.
  woken,
  \* [Threads -> a step name]: where each thread is.
  pc,
  \* Whether a job ever parked (for the witness).
  parkedOnce

\* All of them, for "nothing else changes".
vars == <<list, idle, keeper, batch, running, seen, version, pushed, ran, done, notes, woken, pc,
          parkedOnce>>

\* The start: an empty list, nobody at the step, an empty version.
Init ==
  \* Nothing is handed over, and nobody holds the step.
  /\ list = <<>> /\ idle = TRUE /\ keeper = None
  \* No thread has taken or begun anything.
  /\ batch = [t \in Threads |-> <<>>] /\ running = [t \in Threads |-> None]
  \* No section has read a version.
  /\ seen = [t \in Threads |-> {}]
  \* The version holds no edit, and nothing was handed over or finished.
  /\ version = {} /\ pushed = <<>> /\ ran = <<>> /\ done = {}
  \* No job is parked or told.
  /\ notes = {} /\ woken = {}
  \* A job that keeps the step starts by trying to hold it; a caller starts
  \* by handing its section over.
  /\ pc = [t \in Threads |-> IF t \in Holders THEN "try" ELSE "start"]
  \* Nothing has parked.
  /\ parkedOnce = FALSE

\* ---------------------------------------------------------------------------
\* Handing a section over (Step::run)
\* ---------------------------------------------------------------------------

\* A caller pushes its section. The push that finds nobody holding the step
\* takes it with the same compare-and-swap; any other push leaves the
\* section for the holder and waits for it.
Push(c) ==
  \* A caller at its start.
  /\ c \in Callers /\ pc[c] = "start"
  \* The section is on the list, behind every one pushed before it.
  /\ list' = Append(list, c) /\ pushed' = Append(pushed, c)
  \* Who holds the step now?
  /\ IF idle
       \* Nobody did: this push took it, and this thread runs the list.
       THEN /\ idle' = FALSE
            \* It goes on to take the list.
            /\ pc' = [pc EXCEPT ![c] = "take"]
       \* Somebody does: the caller waits for its section to be run.
       ELSE /\ UNCHANGED idle
            \* It waits for its section's job.
            /\ pc' = [pc EXCEPT ![c] = "wait"]
  \* Nothing else changes.
  /\ UNCHANGED <<keeper, batch, running, seen, version, ran, done, notes, woken, parkedOnce>>

\* A job that keeps the step hands its own section over. It holds the step,
\* so the push takes nothing; it runs what is queued itself, this section
\* last (ST6). The planted bug HolderWaits waits for the section instead,
\* as a caller that does not hold the step does.
Hand(h) ==
  \* A job that holds the step.
  /\ h \in Holders /\ pc[h] = "hand"
  \* Its section is on the list.
  /\ list' = Append(list, h) /\ pushed' = Append(pushed, h)
  \* It runs the list itself, or (the bug) waits.
  /\ pc' = [pc EXCEPT ![h] = IF Mutant = "HolderWaits" THEN "wait" ELSE "take"]
  \* Nothing else changes.
  /\ UNCHANGED <<idle, keeper, batch, running, seen, version, ran, done, notes, woken, parkedOnce>>

\* ---------------------------------------------------------------------------
\* Running the list, as the thread that holds the step (Step::sweep, drain)
\* ---------------------------------------------------------------------------

\* The holder takes everything on the list in one swap, oldest first. The
\* planted bug NewestFirst leaves it in the list's own order, newest first.
TakeList(t) ==
  \* The thread holds the step and is about to run what is queued.
  /\ pc[t] = "take"
  \* The whole list is in its hands, in the order it was pushed.
  /\ batch' = [batch EXCEPT ![t] = IF Mutant = "NewestFirst" THEN Reverse(list) ELSE list]
  \* The list is empty again.
  /\ list' = <<>>
  \* It runs them next.
  /\ pc' = [pc EXCEPT ![t] = "run"]
  \* Nothing else changes.
  /\ UNCHANGED <<idle, keeper, running, seen, version, pushed, ran, done, notes, woken, parkedOnce>>

\* The next section begins: it reads the version it will build on.
Begin(t) ==
  \* Running, between two sections, with one left to run.
  /\ pc[t] = "run" /\ running[t] = None /\ batch[t] # <<>>
  \* The oldest one in its hands is the one it is in.
  /\ running' = [running EXCEPT ![t] = Head(batch[t])]
  \* It read the version as it is now.
  /\ seen' = [seen EXCEPT ![t] = version]
  \* Nothing else changes.
  /\ UNCHANGED <<list, idle, keeper, batch, version, pushed, ran, done, notes, woken, pc, parkedOnce>>

\* The section ends: it installs the version it built, the one it read with
\* its own edit added, and its job lands.
End(t) ==
  \* In the middle of a section.
  /\ pc[t] = "run" /\ running[t] # None
  \* The version is the one it read, with its edit.
  /\ version' = seen[t] \cup {running[t]}
  \* It finished, after every section that finished before.
  /\ ran' = Append(ran, running[t])
  \* Its job lands: its caller may go on.
  /\ done' = done \cup {running[t]}
  \* It leaves the thread's hands.
  /\ batch' = [batch EXCEPT ![t] = Tail(batch[t])]
  \* The thread is between two sections again.
  /\ running' = [running EXCEPT ![t] = None]
  \* Nothing else changes.
  /\ UNCHANGED <<list, idle, keeper, seen, pushed, notes, woken, pc, parkedOnce>>

\* Everything the thread took has run. A job that keeps the step keeps it;
\* any other thread goes to give it up.
BatchDone(t) ==
  \* Running, with nothing left in its hands.
  /\ pc[t] = "run" /\ running[t] = None /\ batch[t] = <<>>
  \* The keeper goes on with its own work; others try to give the step up.
  /\ pc' = [pc EXCEPT ![t] = IF keeper = t THEN "drop" ELSE "rest"]
  \* Nothing else changes.
  /\ UNCHANGED <<list, idle, keeper, batch, running, seen, version, pushed, ran, done, notes, woken,
                 parkedOnce>>

\* The holder gives the step up, with one compare-and-swap that goes in only
\* on an empty list (ST2): a section pushed since the take is run by this
\* thread first. The planted bug GiveUpBlind sets the flag whatever is on
\* the list.
Rest(t) ==
  \* The thread ran everything it took.
  /\ pc[t] = "rest"
  \* Is the list still empty?
  /\ IF list = <<>> \/ Mutant = "GiveUpBlind"
       \* Yes (or not looked at): nobody holds the step.
       THEN /\ idle' = TRUE
            \* It reads the notes next.
            /\ pc' = [pc EXCEPT ![t] = "release"]
       \* No: something was pushed meanwhile, and it runs that first.
       ELSE /\ UNCHANGED idle
            \* Back to taking the list.
            /\ pc' = [pc EXCEPT ![t] = "take"]
  \* Nothing else changes.
  /\ UNCHANGED <<list, keeper, batch, running, seen, version, pushed, ran, done, notes, woken,
                 parkedOnce>>

\* The step given up, the thread reads the notes: every parked job is told
\* (io/parked.rs, PK1). The planted bug ReleaseTellsNobody leaves them.
Release(t) ==
  \* The thread just gave the step up.
  /\ pc[t] = "release"
  \* The notes are read, or not.
  /\ IF Mutant = "ReleaseTellsNobody"
       \* Nobody is told.
       THEN UNCHANGED <<notes, woken>>
       \* Every parked job is told, and the notes are gone.
       ELSE /\ woken' = woken \cup notes
            \* No note is left.
            /\ notes' = {}
  \* A caller goes on to wait for its own section, which it has run by now;
  \* a job that kept the step is through.
  /\ pc' = [pc EXCEPT ![t] = IF t \in Callers THEN "wait" ELSE "returned"]
  \* Nothing else changes.
  /\ UNCHANGED <<list, idle, keeper, batch, running, seen, version, pushed, ran, done, parkedOnce>>

\* A caller returns once its section's job has landed (ST4). The planted
\* bug ReturnEarly returns without it.
Return(c) ==
  \* A thread waiting for its section.
  /\ pc[c] = "wait"
  \* Its section has run (or it does not look).
  /\ c \in done \/ Mutant = "ReturnEarly"
  \* It returns to its caller.
  /\ pc' = [pc EXCEPT ![c] = "returned"]
  \* Nothing else changes.
  /\ UNCHANGED <<list, idle, keeper, batch, running, seen, version, pushed, ran, done, notes, woken,
                 parkedOnce>>

\* ---------------------------------------------------------------------------
\* Keeping the step (Step::hold_or_park, Hold)
\* ---------------------------------------------------------------------------

\* The step is this job's, with the compare-and-swap that clears the flag.
Held(h) ==
  \* Nobody else can take it now.
  /\ idle' = FALSE
  \* The job keeps it.
  /\ keeper' = h
  \* It goes on to its work.
  /\ pc' = [pc EXCEPT ![h] = "hand"]

\* A job tries to take the step: one compare-and-swap on the flag, never a
\* wait. The planted bug HoldWithoutCas takes it whoever holds it.
Try(h) ==
  \* A job about to start work that hands sections over.
  /\ h \in Holders /\ pc[h] = "try"
  \* Is the step free?
  /\ IF idle \/ Mutant = "HoldWithoutCas"
       \* Free (or not looked at): the job holds it.
       THEN Held(h)
       \* Held by another thread: the job will park.
       ELSE /\ UNCHANGED <<idle, keeper>>
            \* It leaves its note first.
            /\ pc' = [pc EXCEPT ![h] = "note"]
  \* Nothing else changes.
  /\ UNCHANGED <<list, batch, running, seen, version, pushed, ran, done, notes, woken, parkedOnce>>

\* The note: "tell me when the step is given up". A step of its own, after
\* the try: the step may be given up in between, and then nobody reads the
\* note. That is what the second try is for.
Note(h) ==
  \* The job found the step held.
  /\ pc[h] = "note"
  \* Its waker is on the step.
  /\ notes' = notes \cup {h}
  \* The second try comes next.
  /\ pc' = [pc EXCEPT ![h] = "look"]
  \* Nothing else changes.
  /\ UNCHANGED <<list, idle, keeper, batch, running, seen, version, pushed, ran, done, woken,
                 parkedOnce>>

\* The second try, with the note in place. The planted bug NoSecondTry
\* parks without it.
Look(h) ==
  \* The job left its note.
  /\ pc[h] = "look"
  \* Is the step free now?
  /\ IF idle /\ Mutant # "NoSecondTry"
       \* Free: the job holds it after all. Its note stays, and tells it
       \* once for nothing.
       THEN /\ Held(h)
            \* Nothing parked.
            /\ UNCHANGED parkedOnce
       \* Still held: the job parks. The poll that ran it returns.
       ELSE /\ UNCHANGED <<idle, keeper>>
            \* It is parked until it is told.
            /\ pc' = [pc EXCEPT ![h] = "parked"]
            \* A job parked: the witness.
            /\ parkedOnce' = TRUE
  \* Nothing else changes.
  /\ UNCHANGED <<list, batch, running, seen, version, pushed, ran, done, notes, woken>>

\* A parked job that was told is run again by its queue's poll, and tries
\* from the top.
Wake(h) ==
  \* Parked, and told.
  /\ pc[h] = "parked" /\ h \in woken
  \* The message is used up.
  /\ woken' = woken \ {h}
  \* Back to the try.
  /\ pc' = [pc EXCEPT ![h] = "try"]
  \* Nothing else changes.
  /\ UNCHANGED <<list, idle, keeper, batch, running, seen, version, pushed, ran, done, notes,
                 parkedOnce>>

\* The job's work is over and it lets the hold go (Hold::drop): it is no
\* longer the keeper, and runs whatever was queued meanwhile before it
\* gives the step up.
Drop(h) ==
  \* The keeper ran everything queued, its own section included.
  /\ pc[h] = "drop"
  \* It keeps the step no longer.
  /\ keeper' = None
  \* It sweeps: take, run, give up.
  /\ pc' = [pc EXCEPT ![h] = "take"]
  \* Nothing else changes.
  /\ UNCHANGED <<list, idle, batch, running, seen, version, pushed, ran, done, notes, woken,
                 parkedOnce>>

\* A job stops for good while it keeps the step: its thread is not
\* scheduled again. Only where a configuration allows it (the stated limit).
StopKeeping(h) ==
  \* It may stop, and it keeps the step with its work still to do.
  /\ h \in MayStop /\ pc[h] = "hand"
  \* It takes no step from here on.
  /\ pc' = [pc EXCEPT ![h] = "stopped"]
  \* Nothing else changes: the step stays held.
  /\ UNCHANGED <<list, idle, keeper, batch, running, seen, version, pushed, ran, done, notes, woken,
                 parkedOnce>>

\* ---------------------------------------------------------------------------
\* The whole
\* ---------------------------------------------------------------------------

\* Every step thread `t` can take.
Steps(t) ==
  \* Hand a section over.
  \/ Push(t) \/ Hand(t)
  \* Run the list as the thread that holds the step.
  \/ TakeList(t) \/ Begin(t) \/ End(t) \/ BatchDone(t) \/ Rest(t) \/ Release(t)
  \* Return once the section has run.
  \/ Return(t)
  \* Take the step to keep it, park, be told, and let it go.
  \/ Try(t) \/ Note(t) \/ Look(t) \/ Wake(t) \/ Drop(t)

\* One step of the run: a thread's step, or a job stopping where it may.
Next == \E t \in Threads : Steps(t) \/ StopKeeping(t)

\* The run: the start, steps, and every thread that can go on does go on.
\* Stopping is never forced, and a stopped job has no step to take.
Spec ==
  \* The start and the steps.
  /\ Init /\ [][Next]_vars
  \* No thread stops for good while it has a step to take.
  /\ \A t \in Threads : WF_vars(Steps(t))

\* ---------------------------------------------------------------------------
\* What must be true
\* ---------------------------------------------------------------------------

\* Every variable holds the kind of thing its comment says.
TypeOK ==
  \* The list and the ghosts hold sections.
  /\ list \in Seq(Threads) /\ pushed \in Seq(Threads) /\ ran \in Seq(Threads)
  \* The flag, and who keeps the step.
  /\ idle \in BOOLEAN /\ keeper \in Holders \cup {None}
  \* What each thread holds and is in.
  /\ batch \in [Threads -> Seq(Threads)] /\ running \in [Threads -> Threads \cup {None}]
  \* What each section read, and the version.
  /\ seen \in [Threads -> SUBSET Threads] /\ version \subseteq Threads
  \* Landed jobs, notes and told jobs.
  /\ done \subseteq Threads /\ notes \subseteq Holders /\ woken \subseteq Holders
  \* Every thread is at a step this model names.
  /\ pc \in [Threads -> {"start", "wait", "take", "run", "rest", "release", "returned", "try", "note",
                         "look", "parked", "hand", "drop", "stopped"}]
  \* The witness flag.
  /\ parkedOnce \in BOOLEAN

\* ST1: at most one thread is in a section at any moment. Rules out the
\* story at the top: the flush and the ingest at the bench together.
OneAtATime == Cardinality({t \in Threads : running[t] # None}) <= 1

\* What ST1 buys: an edit that finished is in the version. Rules out the
\* ingest installing {ingest's table} over the flush's version.
NoLostEdit == done \subseteq version

\* ST2: the step is never given up over a waiting section. Nobody would run
\* it: the next push would, but there may be none.
NoneLeftBehind == idle => list = <<>>

\* ST3: sections finish in the order they were handed over.
InPushOrder ==
  \* No more finished than were handed over ...
  /\ Len(ran) <= Len(pushed)
  \* ... and the n-th to finish is the n-th handed over.
  /\ \A i \in 1..Len(ran) : ran[i] = pushed[i]

\* ST4: a thread that returned has had its section run to its end.
FinishedBeforeReturn == \A t \in Threads : pc[t] = "returned" => t \in done

\* ST6: a job that keeps the step never waits for another thread. It parks
\* (the poll returns), or it runs its section itself.
NoHolderWaits == \A h \in Holders : pc[h] # "wait"

\* The witness: no job ever parks. False on purpose: the GREEN run with
\* this added must break, which shows it does go through a park.
NeverParked == ~parkedOnce

\* ST2 and ST6 as one promise about whole runs: every section runs. With a
\* section pushed just as the holder gives the step up, and with a job that
\* parked for the step just as it was given up.
EverySectionRuns == \A t \in Threads : <>(t \in done)

====
