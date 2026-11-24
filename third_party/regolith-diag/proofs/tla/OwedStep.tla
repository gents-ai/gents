---- MODULE OwedStep ----
\* ===========================================================================
\* THE STORY, for a reader who has never seen the code
\* ===========================================================================
\* regolith keeps new writes in memory, in a "memtable". When the memtable is
\* full it is "sealed" and has to be written out to a file: a "flush". The
\* files a flush writes land in "L0", the top level of the tree. When L0
\* holds too many files they are merged into the next level: a "compaction".
\* Every read looks at every L0 file, so a growing L0 makes every read
\* slower, and past a limit the database stops accepting writes.
\*
\* Usually a background thread, the "worker", flushes and compacts. A
\* database opened with no worker (for example on a wasm target that has no
\* threads) has nobody to do that. So the writes do it themselves:
\*   - a write that seals a memtable leaves one step of background work
\*     "owed";
\*   - the next write to come out of the commit pipeline starts exactly ONE
\*     step, then returns to its caller;
\*   - one step is one flush of the oldest sealed memtable, or, when nothing
\*     is sealed, one compaction pass;
\*   - a step that did work owes the next step, and so does every flush.
\* That way the writes pace the background work and no write pays for more
\* than one step.
\*
\* Tiny example, with a memtable of two writes and a trigger of two files:
\*   write 2 seals, and its step flushes:            L0 = 1
\*   write 3 runs a pass, which finds nothing to do:  L0 = 1
\*   write 4 seals, and its step flushes:            L0 = 2, the trigger
\*   write 5 runs a pass, which compacts L0:          L0 = 0
\* L0 never passes 2, and no write ran more than one step.
\*
\* WHERE THE STEP RUNS, and why it decides which bound holds. A writer with no
\* IoQueue open runs the step inline, before its write returns: each owed
\* step is done before the next write, as in the example. A writer with a
\* queue open leaves the step there as a job and returns at once; the job
\* runs when the writer polls its queue, and while it waits every later
\* write's debt rides on that same job (`owed_queued`). A writer that keeps
\* writing without polling then seals memtable after memtable, and the
\* rotation, which keeps at most `max_write_buffer_number` memtables, flushes
\* the older sealed one itself to make room. Each such flush adds a file to
\* L0 and nobody compacts:
\*   writes 1-2 seal memtable A, a job waits:         L0 = 0, A sealed
\*   writes 3-4 seal B; the rotation flushes A first: L0 = 1, B sealed
\*   writes 5-6: flush B, seal C:                     L0 = 2, the trigger
\*   writes 7-8: flush C, seal D:                     L0 = 3, over it
\* So the tight bound (L0 at most the compaction trigger) and "each write
\* runs exactly the step it owed" hold only when every owed step completes
\* before the next write: inline execution, or a queue polled after every
\* write. With the queue the bound is the stop trigger's, plus the sealed
\* memtables a rotation keeps (`max_write_buffer_number - 1`): writes stop
\* at the stop trigger, and a step relieving the stop flushes what is still
\* sealed before it compacts.
\*
\* WHAT WENT WRONG BEFORE (E16). A flush owed nothing. In the example, write
\* 5 would then run no step at all (nothing owed), write 6 would seal and
\* flush, and L0 would be 3, then 4, and so on until the database stopped
\* writes. The other way to get it wrong is a write that keeps running steps
\* until nothing is owed: the caller of one small write then waits for a
\* flush, a compaction and whatever follows.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/OwedStep.lean. The
\* first three are about inline execution, each owed step done before the
\* next write:
\*   write_one_step           a write runs one step when one is owed as it
\*                            leaves the pipeline, none otherwise
\*                            (OneStepPerWrite, ExactlyTheOwedStep here)
\*   write_inv, l0_bounded    with at least two writes per memtable, L0
\*                            never passes its trigger and no sealed
\*                            memtable is left waiting (L0Bounded,
\*                            FrozenBounded here, for any sizes)
\*   stopped_write_inv, l0_bounded_by_stop
\*                            for any memtable size, L0 never passes the
\*                            stop trigger
\*   queued_l0_bounded        on the queue, with the steps run whenever the
\*                            writer polls (or never), L0 never passes the
\*                            stop trigger plus the sealed memtables a
\*                            rotation keeps (L0WithinStopAndRoom,
\*                            FrozenWithinRoom here)
\*   flush_owes_nothing_overfills, step_until_idle_runs_three,
\*   queued_passes_trigger    the RED cases, as counterexamples
\*
\* THE ENGINE (src/engine/background_step.rs unless said otherwise).
\*   RegolithEngine::flush_sealed_later   a seal with no worker owes a step
\*   RegolithEngine::after_flush          so does every flush (E16)
\*   RegolithEngine::run_owed_step        called by a write once its commit
\*     returned (submit_settled, src/engine/commit/mod.rs). With no queue
\*     open it runs `run_owed` inline; with one, it leaves an `Owed` job on
\*     it unless one waits already (`owed_queued`), and the job runs
\*     `run_owed` when the writer polls. `run_owed` takes the debt and runs
\*     one `run_one_background_step`: a flush of the oldest sealed memtable,
\*     else one compaction pass. `DidWork` owes the next step; `Idle` owes
\*     nothing.
\*   Turn::rotation_due, rotate, flush_down_to  src/engine/commit/turn.rs: a
\*     rotation that would keep more than `max_write_buffer_number`
\*     memtables flushes the oldest sealed ones first, in the pipeline.
\*   pick_and_run_level_compaction        src/engine/compaction.rs: a pass
\*     looks at L0 first, and `compact_l0` merges all of L0 into L1.
\*   RegolithEngine::relieve_inline       src/engine/mod.rs: a write stopped
\*     because L0 reached the stop trigger runs steps until the stop clears.
\*
\* THE DEFECTS, one per mutant value.
\*   "FlushOwesNothing"  a step that flushed leaves nothing owed (the engine
\*                       before E16).
\*   "StepUntilIdle"     a write keeps running steps while one is owed.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - Several threads writing at once. A step can then find another thread
\*     flushing or compacting, do nothing, and owe again; and writes can
\*     outrun the one thread that works. The bound on L0 is then the stop
\*     trigger's, and how a stop is relieved is StallRelief.tla. Here one
\*     write runs at a time: a database with no worker used from one thread.
\*   - How the job reaches the queue and how a poll runs it (NonBlocking.tla):
\*     here a job either waits or has run.
\*   - What the deeper levels hold. A pass that leaves L0 alone may still
\*     find work below it; the model lets it go either way (see RunOwed).
\*   - A step that fails. It owes nothing more until the next seal, and the
\*     failure is reported where the step records it.
\*   - The bytes: a memtable is "full" after SealEvery writes.
\*
\* CONFIGURATIONS.
\*   MC_OwedStep_Green                   inline, two writes per memtable: L0
\*                                       stays at its trigger
\*   MC_OwedStep_Green_TinyMemtable      inline, every write seals: L0 stays
\*                                       within the stop trigger
\*   MC_OwedStep_Green_Queued            on the queue: L0 stays within the
\*                                       stop trigger plus one sealed memtable
\*   MC_OwedStep_Red_FlushOwesNothing    L0Bounded fails
\*   MC_OwedStep_Red_StepUntilIdle       OneStepPerWrite fails
\*   MC_OwedStep_Red_QueuedTightBound    on the queue, L0Bounded fails: the
\*                                       tight bound needs the inline step

\* Borrow the standard definitions of numbers.
EXTENDS Naturals

\* The knobs of a run. Each configuration file (MC_OwedStep_*.cfg) gives them values.
CONSTANTS
  \* How many writes fill a memtable: the write that fills it seals it. It stands for
  \* `write_buffer_size` (src/options.rs), counted in writes.
  SealEvery,
  \* How many L0 files make a pass compact L0: `l0_compaction_trigger` (src/options.rs).
  Trigger,
  \* How many L0 files stop writes: `level0_stop_writes_trigger` (src/options.rs).
  Stop,
  \* How many sealed memtables a rotation keeps: `max_write_buffer_number - 1` (src/options.rs,
  \* Turn::rotation_due in src/engine/commit/turn.rs). The default keeps one.
  Room,
  \* TRUE when the writing thread has an IoQueue open, so the owed step is left there as a job;
  \* FALSE when it runs inline, before the write returns (run_owed_step).
  Queued,
  \* "none" for the design, or the name of one planted bug.
  Mutant

\* TLC refuses a configuration with a memtable that holds no write,
ASSUME SealEvery \in Nat \ {0}
\* with a trigger of no file,
ASSUME Trigger \in Nat \ {0}
\* with a stop trigger below the compaction trigger,
ASSUME Stop \in Nat /\ Stop >= Trigger
\* with a rotation that keeps no sealed memtable (that rotation flushes inline, another path),
ASSUME Room \in Nat \ {0}
\* with a queue that is neither open nor closed,
ASSUME Queued \in BOOLEAN
\* or that names no known bug.
ASSUME Mutant \in {"none", "FlushOwesNothing", "StepUntilIdle"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* How many writes the active memtable holds (MemTable, src/engine/memtable.rs). A number.
  filled,
  \* How many sealed memtables wait for their flush: the frozen list of the read view
  \* (ReadView::frozen, src/engine/read_view.rs). A number.
  frozen,
  \* How many files L0 holds (Version::levels[0], src/engine/manifest.rs). A number.
  l0,
  \* TRUE when one step of background work is owed: the OWED_STEP bit of RegolithEngine::owed, an
  \* atomic byte (src/engine/background_step.rs).
  owed,
  \* Where the one writer is: "idle" (between writes) or "left" (its commit returned and it has
  \* yet to run what is owed).
  pc,
  \* A ghost (bookkeeping the program does not have; it only lets the promises be stated): how
  \* many steps the current write has run since its commit returned.
  ran,
  \* A ghost: whether a step was owed at the moment the current write's commit returned.
  owedAtLeave,
  \* TRUE while an Owed job waits on the writer's queue: RegolithEngine::owed_queued, an atomic
  \* bool (src/engine/background_step.rs). Always FALSE when Queued is FALSE.
  queued

\* Every variable, so a step that changes none of them is a stutter.
vars == <<filled, frozen, l0, owed, pc, ran, owedAtLeave, queued>>

----------------------------------------------------------------------------
\* Actions.

\* The start: an empty memtable, nothing sealed, an empty L0, nothing owed, no write under way.
Init ==
  \* The memtable is empty.
  /\ filled = 0
  \* Nothing is sealed.
  /\ frozen = 0
  \* L0 is empty.
  /\ l0 = 0
  \* Nothing is owed.
  /\ owed = FALSE
  \* No write is under way.
  /\ pc = "idle"
  \* No step was run.
  /\ ran = 0
  \* Nothing was owed.
  /\ owedAtLeave = FALSE
  \* No job waits on the queue.
  /\ queued = FALSE

\* A write goes through the commit pipeline. The write that fills the memtable seals it and
\* swaps in a fresh one; with no worker, that seal owes one step (flush_sealed_later). When Room
\* sealed memtables already wait, the rotation first flushes the oldest of them, in the pipeline,
\* until Room - 1 are left (flush_down_to); each such flush owes a step too (after_flush).
Commit ==
  \* When: no write is under way,
  /\ pc = "idle"
  \* and writes are not stopped.
  /\ l0 < Stop
  \* Does this write fill the memtable?
  /\ LET seals == filled + 1 = SealEvery
         \* How many sealed memtables the rotation flushes before it seals: down to Room - 1.
         flushed == IF seals /\ frozen >= Room THEN frozen - Room + 1 ELSE 0
     \* The memtable: fresh after a seal, else one write fuller.
     IN /\ filled' = IF seals THEN 0 ELSE filled + 1
        \* The flushed ones leave the sealed memtables, and a seal adds one.
        /\ frozen' = IF seals THEN frozen - flushed + 1 ELSE frozen
        \* Each flushed one is a file more in L0.
        /\ l0' = l0 + flushed
        \* A seal owes a step; an earlier debt stays.
        /\ owed' = (owed \/ seals)
        \* The ghost notes whether a step is owed as the commit returns.
        /\ owedAtLeave' = (owed \/ seals)
  \* The write has run no step yet.
  /\ ran' = 0
  \* Its commit has returned; what is owed comes next.
  /\ pc' = "left"
  \* The queue is untouched by a commit.
  /\ UNCHANGED queued

\* One step of owed work, its debt already taken (run_owed): a flush if a sealed memtable waits,
\* else a compaction pass. A step that did work owes the next one. The bug FlushOwesNothing
\* forgets the debt after a flush. It changes frozen, l0 and owed, and nothing else.
StepOnce ==
  \* Does a sealed memtable wait?
  IF frozen > 0
    \* Yes: flush the oldest. One sealed memtable fewer,
    THEN /\ frozen' = frozen - 1
         \* one file more in L0,
         /\ l0' = l0 + 1
         \* and the flush owes the next step (the bug owes nothing).
         /\ owed' = (Mutant # "FlushOwesNothing")
    \* No: a compaction pass. Is L0 at its trigger?
    ELSE IF l0 >= Trigger
           \* Yes: the pass merges all of L0 into the next level,
           THEN /\ l0' = 0
                \* and, having done work, owes the next step.
                /\ owed' = TRUE
                \* Nothing is sealed.
                /\ UNCHANGED frozen
           \* No: L0 is left alone. The pass either found work in a deeper level
           \* (and owes the next step) or found nothing (and owes nothing).
           ELSE /\ owed' \in BOOLEAN
                \* L0 and the sealed memtables are untouched.
                /\ UNCHANGED <<frozen, l0>>

\* The write starts what is owed (run_owed_step). Nothing owed: it returns. A step owed, inline:
\* it takes the debt, runs ONE step, and returns; the bug StepUntilIdle goes round again while a
\* step is owed. A step owed, with a queue: it leaves an Owed job there, unless one waits
\* already and will take this debt too, and returns without running anything.
RunOwed ==
  \* When: the write's commit has returned.
  /\ pc = "left"
  \* Is a step owed?
  /\ IF ~owed
       \* Nothing owed: the write returns, having changed nothing.
       THEN /\ pc' = "idle"
            \* No step ran, and no job was left.
            /\ UNCHANGED <<frozen, l0, owed, ran, queued>>
       \* A step is owed. Does the writer have a queue?
       ELSE IF Queued
              \* Yes: one job waits on it now (the one this left, or the one already there),
              THEN /\ queued' = TRUE
                   \* and the write returns at once,
                   /\ pc' = "idle"
                   \* having run no step and changed nothing else.
                   /\ UNCHANGED <<frozen, l0, owed, ran>>
              \* No: the step runs here.
              ELSE /\ StepOnce
                   \* One more step ran in this write.
                   /\ ran' = ran + 1
                   \* The design returns after the one step; the bug StepUntilIdle goes round
                   \* again while a step is owed.
                   /\ pc' = IF Mutant = "StepUntilIdle" /\ owed' THEN "left" ELSE "idle"
                   \* No queue, so no job.
                   /\ UNCHANGED queued
  \* The memtable and the ghost of the commit are untouched.
  /\ UNCHANGED <<filled, owedAtLeave>>

\* The writer polls its queue between two writes and the Owed job runs (Owed::run): it marks that
\* no job waits, then runs what is owed now, which is every debt the writes since it was left
\* put on it: one step, or nothing if none is owed. A writer may poll at any time, or never: the
\* model lets a poll come between any two writes, so every schedule of polls is checked.
Poll ==
  \* When: no write is under way,
  /\ pc = "idle"
  \* and a job waits on the queue.
  /\ queued
  \* The job takes itself off: a later write leaves a new one.
  /\ queued' = FALSE
  \* A step is owed: run one. Nothing owed: nothing changes.
  /\ IF owed THEN StepOnce ELSE UNCHANGED <<frozen, l0, owed>>
  \* The memtable, the writer and the ghosts are untouched.
  /\ UNCHANGED <<filled, pc, ran, owedAtLeave>>

\* A write that finds L0 at the stop trigger is stopped, and the stop is relieved on its own
\* thread: inline (relieve_inline), or by the StallStep job its polls run when it has a queue.
\* Either way it is the same background step, a flush if a sealed memtable waits, else a
\* compaction pass, which at the stop trigger always has L0 to merge.
Relieve ==
  \* When: no write is under way,
  /\ pc = "idle"
  \* and writes are stopped.
  /\ l0 >= Stop
  \* Does a sealed memtable wait?
  /\ IF frozen > 0
       \* Yes: flush the oldest. One sealed memtable fewer,
       THEN /\ frozen' = frozen - 1
            \* one file more in L0,
            /\ l0' = l0 + 1
            \* and the flush owes a step.
            /\ owed' = TRUE
       \* No: the pass merges all of L0 away (the stop trigger is not below the compaction
       \* trigger).
       ELSE /\ l0' = 0
            \* Nothing is sealed, and the debt is as it was.
            /\ UNCHANGED <<frozen, owed>>
  \* The memtable, the writer, the ghosts and the queue are untouched.
  /\ UNCHANGED <<filled, pc, ran, owedAtLeave, queued>>

\* Every step the system can take.
Next ==
  \* A write commits, or starts what is owed, or a stopped write relieves the stop, or the
  \* writer polls its queue.
  \/ Commit \/ RunOwed \/ Relieve \/ Poll

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* The most files L0 may ever hold when every owed step runs before the next write (inline).
\* With at least two writes per memtable the write after the flush that fills L0 does not seal,
\* so its step is the compaction: the bound is the compaction trigger. When every write seals,
\* every step is a flush, and it is the stop trigger that makes a write compact: the bound is the
\* stop trigger.
L0Limit == IF SealEvery >= 2 THEN Trigger ELSE Stop

\* Every variable holds what its comment says.
TypeOK ==
  \* The memtable is never fuller than full.
  /\ filled \in 0..(SealEvery - 1)
  \* The sealed memtables are counted by a number.
  /\ frozen \in Nat
  \* So are the L0 files.
  /\ l0 \in Nat
  \* A step is owed or not.
  /\ owed \in BOOLEAN
  \* The writer is between writes or after its commit.
  /\ pc \in {"idle", "left"}
  \* The steps a write ran are counted by a number.
  /\ ran \in Nat
  \* A step was owed at the commit's return or not.
  /\ owedAtLeave \in BOOLEAN
  \* A job waits on the queue or not.
  /\ queued \in BOOLEAN

\* A write runs at most one step. Example ruled out: the write that seals a memtable flushes it,
\* then compacts L0, then runs a third pass, all before it returns to a caller that asked for
\* one small write. The program needs this because the step is paid on the caller's own thread:
\* it must be bounded. Lean: write_one_step.
OneStepPerWrite ==
  \* The steps of the current write are 0 or 1.
  ran <= 1

\* A write that has returned ran exactly what was owed when its commit returned: one step if a
\* step was owed, none if not. Example ruled out: a step is owed and the write returns without
\* running it, so the sealed memtable sits in memory until some later write. Lean: write_one_step.
\* Inline only: a writer with a queue leaves the step there and runs none, by design.
ExactlyTheOwedStep ==
  \* Once the write has returned,
  pc = "idle" =>
    \* it ran one step if one was owed, and none otherwise.
    ran = (IF owedAtLeave THEN 1 ELSE 0)

\* THE HEADLINE, for inline execution. L0 never holds more files than its limit. Example ruled
\* out: a trigger of 2, and an L0 of 3 files because the write after the flush that filled L0 ran
\* no compaction. The program needs this because every read looks at every L0 file. Lean:
\* l0_bounded, l0_bounded_by_stop. It needs each owed step done before the next write: with a
\* queue nobody polls, L0 passes the trigger (MC_OwedStep_Red_QueuedTightBound).
L0Bounded ==
  \* The L0 files are within the limit.
  l0 <= L0Limit

\* At most one sealed memtable waits for its flush: the write that seals one flushes it before it
\* returns. Example ruled out: sealed memtables piling up in memory because nobody flushes them.
\* Lean: l0_bounded (its second half). Inline only, like L0Bounded.
FrozenBounded ==
  \* Zero or one sealed memtable waits.
  frozen <= 1

\* THE BOUND ON THE QUEUE, however the writer polls. Writes stop at the stop trigger, and the
\* write that brought L0 there may have left up to Room sealed memtables, which the steps that
\* relieve the stop flush before they compact: L0 never holds more than the stop trigger plus
\* Room files. Example ruled out: L0 growing past the stop trigger because the rotation's own
\* flushes ignored it. Lean: queued_l0_bounded.
L0WithinStopAndRoom ==
  \* The L0 files are within the stop trigger plus what a rotation keeps sealed.
  l0 <= Stop + Room

\* Never more than Room sealed memtables wait: a rotation flushes the oldest first to keep it so.
\* Example ruled out: memory growing with sealed memtables while the writer never polls. Lean:
\* queued_l0_bounded.
FrozenWithinRoom ==
  \* At most Room sealed memtables wait.
  frozen <= Room

====
