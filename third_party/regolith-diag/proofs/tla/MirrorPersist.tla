---- MODULE MirrorPersist ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A database opened on a browser's main thread runs in OPFS mirror mode:
\* every file lives in memory (MirrorFs, src/env/opfs/mirror.rs), and nothing
\* is durable until the app calls OpfsEnv::persist (src/env/opfs/mod.rs),
\* which writes every dirty file back to OPFS whole. One OPFS write is a
\* chain of browser promises (getFileHandle, createWritable, write, close);
\* the close is what swaps the new bytes in. A file removed from the mirror
\* is owed a removal (MirrorFs::deleted), which persist makes with
\* removeEntry. The next mount reads whatever OPFS holds (MirrorFs::load).
\*
\* One persist call is one batch: capture the dirty files and the owed
\* names, write the files (tables first, then every other file, then the
\* MANIFEST: persist_rank, src/env/persist_order.rs, E20), remove the owed
\* names, and settle: count each version written as persisted, and forget
\* the owed names it removed. Every browser call is a promise the batch
\* awaits, and other code runs meanwhile: the engine, and another persist.
\*
\* The rules this model checks (regolith #289 added the first three):
\*   - the lock: batches run one at a time. The batch takes persist_lock (a
\*     crate::sync::Mutex) before it captures, and holds it across every
\*     browser await until it ends (persist_batch);
\*   - the keep-alive: the batch runs as a promise of its own
\*     (future_to_promise in MirrorFs::persist), so a caller that drops its
\*     future drops neither the batch nor the lock it holds while a browser
\*     call of the batch is still in flight;
\*   - the classification: a removal that fails with anything but
\*     NotFoundError fails the call before settle, so its name stays owed;
\*     NotFoundError means the entry is already gone, and counts as removed;
\*   - the order: tables before the MANIFEST that names them, and the
\*     removals last, made only once every write of the batch went through.
\*
\* Tiny example of what the lock and the keep-alive prevent (the browser test
\* overlapping_persist_cannot_publish_a_retired_manifest,
\* tests/wasm_opfs_main/persist.rs). Batch A writes MANIFEST version 2, which
\* names tables T1 and T2; its close is still in flight. A compaction writes
\* T3 and MANIFEST version 3 and removes T1 and T2. Batch B starts, writes T3
\* and MANIFEST 3, and removes T1 and T2. Then A's close lands: OPFS holds
\* MANIFEST 2 again, which names two tables OPFS no longer has, and the
\* mirror counts MANIFEST 3 as persisted, so nothing writes it again. Before
\* #289 nothing kept B from starting; now B waits for the lock, and a caller
\* that drops A's future no longer lets go of the lock under A's close.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - OPFS's MANIFEST goes back to an older version (INV_ManifestMonotone);
\*   - the mirror counts a version as persisted that OPFS does not hold
\*     (INV_CleanIsStored), so no later call writes it;
\*   - a batch runs while a browser call of another batch is in flight
\*     (INV_SerialBatches): a dropped call's batch finishes before the next
\*     one starts;
\*   - an OPFS entry for a name the mirror no longer holds is no longer owed
\*     a removal, so it is never removed (INV_RemovalOwed);
\*   - OPFS's MANIFEST names a table OPFS does not hold
\*     (INV_ManifestNamesStored), at any moment, so a tab closed, or a call
\*     failed, at any point leaves a database that opens;
\*   - and, over whole runs: while the app keeps calling persist, the mirror
\*     and OPFS end up the same, every file written and every removal made
\*     (LIVE_CatchesUp).
\*
\* Why the MANIFEST version OPFS holds never goes down for runs of any
\* length, why a failed removal stays owed for any number of names, and why
\* a batch in persist order cut short anywhere keeps every table before the
\* MANIFEST and every write before a removal, for batches of any size:
\* proofs/lean/Regolith/MirrorPersist.lean.
\*
\* WHAT IS MODELLED, and the code each piece mirrors (file and line of the
\* tree the model was written against; src/env/opfs/mirror.rs unless named):
\*   Variable  Rust
\*   stage     the engine's run: open (T1, MANIFEST 1), a second flush (T2,
\*             MANIFEST 2), a compaction (T3, MANIFEST 3, T1 and T2 removed).
\*             The mirror sees MirrorFs::create (386-401), write_at (360-366)
\*             and remove_file (430-438).
\*   Ver(f)    Entry::version (93), raised by Entry::touch (109-111) with a
\*             number from MirrorFs::version (274-276); 0 for no entry.
\*   pers      Entry::persisted (97); Dirty(f) is Entry::is_dirty (113-115).
\*   deleted   MirrorFs::deleted (127): insert at remove_file (436), forget
\*             at settle (309).
\*   holder    MirrorFs::persist_lock (130, made at 218).
\*   pc        where each persist call is in MirrorFs::persist (315-325) and
\*             persist_batch (327-354).
\*   todo      the batch's `writes` (329, sorted at 295) then its `deletes`
\*             (296, 329), the calls not made yet.
\*   wrote     `written` (336, pushed at 339).
\*   dels      `deletes` as captured (296), handed to settle (352).
\*   inflight  the browser call that changes OPFS: a write's close
\*             (js::write_whole_file, src/env/opfs/js.rs:263-264) or
\*             removeEntry (js::remove_entry, js.rs:101-104).
\*   disk      the OPFS directory: what MirrorFs::load (239-260) reads.
\*   faults    a browser call that fails, as failRemovals does in
\*             tests/wasm_opfs_main/persist.js.
\*   doomed    only the bug DeleteAfterFailedWrite uses it.
\*   peak, failed  ghosts: the newest MANIFEST OPFS ever held; some call
\*             returned an error.
\*
\*   Action    Rust
\*   Start     OpfsEnv::persist (mod.rs:322-331) calls MirrorFs::persist
\*             (315-325), which starts the batch as its own promise
\*             (future_to_promise, 319-322).
\*   Acquire   persist_batch: `self.persist_lock.lock().await` (328), then
\*             take_persist_batch (329; 282-298), no await between.
\*   CaptureEarly
\*             the bug CaptureFirst only: take_persist_batch moved above
\*             the lock, so the first poll captures and then parks on a
\*             held lock.
\*   Issue     the write loop (337-340) calling js::write_whole_file (338),
\*             up to the close it awaits; or the removal loop (341-350)
\*             calling js::remove_entry (344).
\*   Land      the browser finishes the call and the batch resumes: Ok, and
\*             `written.push` (339); a write's error, returned by `?` (338);
\*             a removal's error, read by name (345): NotFoundError counted
\*             as removed (346), any other returned (347).
\*   Finish    settle (352; 302-311): persisted.fetch_max (305) for a file
\*             the mirror still holds (304), deleted.remove (309); Ok (353),
\*             the lock's guard dropped as the batch returns. An empty batch
\*             returns at 330-332, which is a settle of nothing.
\*   Cancel    the caller drops its future: the JsFuture of 323. The promise
\*             of 319 keeps the batch, and its guard, going.
\*   Flush     a flush on the main thread, one synchronous call between two
\*             awaits: its table (create, write_at) and the edit it appends
\*             to the MANIFEST (write_at).
\*   Compact   a compaction, one synchronous call: its output table, its
\*             edit (versions.apply, src/engine/compaction.rs:1682), then
\*             its inputs removed (delete_old_files, compaction.rs:1692-1697,
\*             to MirrorFs::remove_file).
\*
\*   Promise                  The guard in Rust that keeps it
\*   INV_ManifestMonotone     the lock taken before the capture (328, 329)
\*                            and held to the batch's end; the keep-alive
\*                            (319) keeping it held when the caller leaves.
\*   INV_CleanIsStored        the same, and settle only once every call of
\*                            the batch came back (352).
\*   INV_SerialBatches        the lock and the keep-alive.
\*   INV_RemovalOwed          the classification (344-349); settle skipped
\*                            on an error (347 returns before 352).
\*   INV_ManifestNamesStored  the sort (295, persist_rank at
\*                            persist_order.rs:17-25); the removal loop
\*                            after the write loop (341 after 337-340); a
\*                            failed write returning before any removal (338).
\*   LIVE_CatchesUp           NotFoundError counted as removed (346); every
\*                            failure leaving its work dirty or owed (no
\*                            settle); the caller contract: the app calls
\*                            persist again (mod.rs:310-321).
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - The first calls of a write (getFileHandle, createWritable, write)
\*     change nothing a mount reads until the close swaps the bytes in, so
\*     Issue stands for all of them. One exception: getFileHandle with create
\*     makes an empty entry for a name OPFS did not have. An empty table no
\*     stored MANIFEST names is an orphan (src/engine/orphan_sweep.rs), an
\*     empty MANIFEST reads as a new one (src/engine/manifest.rs:784-786),
\*     and the name stays dirty or owed, so a later call fills or removes it.
\*   - Files of rank 1 (logs, MANIFEST.tmp, LOCK): written between the tables
\*     and the MANIFEST and removed after it, like a table. A log the stored
\*     MANIFEST still needs is a table it names here.
\*   - The order among tables, and among removals: the map's order. No rule
\*     depends on it, so the model fixes one.
\*   - The version read before the bytes (291-292): on one browser thread
\*     nothing runs inside take_persist_batch, so Acquire captures in one
\*     step. With threads, the version first makes the bytes newer, never
\*     older, so the file stays dirty.
\*   - A name removed and made again while a batch awaits: the new entry's
\*     version comes from the one counter (274-276), above every version a
\*     batch captured, so settle's fetch_max leaves it dirty. Tables here are
\*     never made again: regolith never reuses a file number.
\*   - A rename (443-493) runs whole between two awaits, and like create
\*     (398) it takes a name out of `deleted` before the name holds a file
\*     (454), as the engine's steps here keep `deleted` and the mirror's
\*     files apart.
\*   - mount_directory failing (333-334): the call ends before any browser
\*     call, as a first write that fails ends it.
\*   - The lock's order: crate::sync::Mutex barges with a bounded bypass
\*     (proofs/tla/Sync.tla). Here any waiting call may take a free lock,
\*     which allows every order the mutex allows, and more.
\*   - How many calls: two slots, each starting a new call once its call
\*     ended. That is every race between two batches, and a call made after
\*     one ended (a slot used again).
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one promise
\* it names):
\*   MC_MirrorPersist_Green              the code: every promise holds, and
\*                                       the mirror and OPFS end up the same.
\*   MC_MirrorPersist_Red_NoLock         no lock: an older MANIFEST lands
\*                                       over a newer one:
\*                                       INV_ManifestMonotone breaks.
\*   MC_MirrorPersist_Red_NoLock_Clean   the same bug, and the newer one is
\*                                       counted persisted:
\*                                       INV_CleanIsStored breaks.
\*   MC_MirrorPersist_Red_CaptureFirst   the batch captured before the lock,
\*                                       then parked on it while another
\*                                       batch held it: INV_ManifestMonotone
\*                                       breaks.
\*   MC_MirrorPersist_Red_NoKeepAlive    a dropped caller drops its batch and
\*                                       the lock under a call in flight:
\*                                       INV_SerialBatches breaks.
\*   MC_MirrorPersist_Red_NoKeepAlive_Manifest
\*                                       the same bug, and the dropped
\*                                       batch's old MANIFEST lands last:
\*                                       INV_ManifestMonotone breaks.
\*   MC_MirrorPersist_Red_SwallowRemoval a failed removal counted as done, as
\*                                       before #289: INV_RemovalOwed breaks.
\*   MC_MirrorPersist_Red_NotFoundFails  NotFoundError counted as a failure:
\*                                       LIVE_CatchesUp breaks.
\*   MC_MirrorPersist_Red_ManifestFirst  the MANIFEST written before its
\*                                       tables: INV_ManifestNamesStored
\*                                       breaks.
\*   MC_MirrorPersist_Red_DeleteFirst    the removals before the writes:
\*                                       INV_ManifestNamesStored breaks.
\*   MC_MirrorPersist_Red_DeleteAfterFailedWrite
\*                                       a failed write goes on to the
\*                                       removals: INV_ManifestNamesStored
\*                                       breaks.
\*   MC_MirrorPersist_Red_NoRetry        the caller stops calling persist
\*                                       after a failed call: LIVE_CatchesUp
\*                                       breaks.
\*   MC_MirrorPersist_Green_ReachesWaitBehindOlder
\*                                       the code with the witness
\*                                       NeverWaitsBehindOlder, which must
\*                                       break: a call waits while an older
\*                                       MANIFEST is being written.

\* Numbers and sequences.
EXTENDS Naturals, Sequences

\* The fixed inputs of a configuration.
CONSTANTS
  \* "none" for the code as it is, or the name of one planted bug.
  Mutant,
  \* How many browser calls may fail in one run.
  MaxFaults

\* The bug names this model knows.
ASSUME Mutant \in {"none", "NoLock", "CaptureFirst", "NoKeepAlive", "SwallowRemoval",
                  "NotFoundFails", "ManifestFirst", "DeleteFirst", "DeleteAfterFailedWrite",
                  "NoRetry"}

\* The failure budget is a count.
ASSUME MaxFaults \in Nat

\* ---------------------------------------------------------------------------
\* The files, the calls, and what the MANIFEST names
\* ---------------------------------------------------------------------------

\* Two slots for persist calls: each makes one call at a time, and a new one
\* once its call ended.
Tasks == {1, 2}

\* The tables of the run: T1 and T2 written by flushes, T3 by the compaction
\* that merges them.
Tables == {"T1", "T2", "T3"}

\* Every file: the tables and the MANIFEST, "M".
Files == Tables \cup {"M"}

\* The order of a batch's writes: persist_rank puts tables (0) before the
\* MANIFEST (2); among tables the map's order, fixed here.
Order == <<"T1", "T2", "T3", "M">>

\* The order the bug ManifestFirst writes in: the MANIFEST first.
OrderManifestFirst == <<"M", "T1", "T2", "T3">>

\* The order a batch removes owed names in: the map's order, fixed here.
RemovalOrder == <<"T1", "T2", "T3">>

\* The tables MANIFEST version v names; version 0 is no MANIFEST at all.
Names(v) ==
  \* After the open: the first flush's table.
  CASE v = 1 -> {"T1"}
    \* After the second flush: both tables.
    [] v = 2 -> {"T1", "T2"}
    \* After the compaction: its output only.
    [] v = 3 -> {"T3"}
    \* No MANIFEST in OPFS: it names nothing.
    [] OTHER -> {}

\* The larger of two numbers.
Max(a, b) == IF a >= b THEN a ELSE b

\* What a slot has in flight when it has no browser call in flight.
NoOp == [k |-> "none", f |-> "none", v |-> 0]

\* Every call a batch makes: write file f at version v ("w"), or remove the
\* entry of table f ("r", with v = 0: no entry after it).
Ops == [k : {"w"}, f : Files, v : 1..3] \cup [k : {"r"}, f : Tables, v : {0}]

\* A batch that has written nothing yet.
NoWrites == [f \in Files |-> 0]

\* The state that changes from step to step.
VARIABLES
  \* The engine's progress: 0 after the open (T1 and MANIFEST 1 in memory),
  \* 1 after the second flush (T2, MANIFEST 2), 2 after the compaction (T3,
  \* MANIFEST 3, T1 and T2 removed).
  stage,
  \* Entry::persisted of each file: the newest version a batch wrote and
  \* settled; 0 for a file the mirror holds no entry for.
  pers,
  \* MirrorFs::deleted: the names owed a removal from OPFS.
  deleted,
  \* Who holds persist_lock: a slot, or 0 while it is free.
  holder,
  \* Where each slot's call is: "idle" (none), "wait" (called, its batch
  \* not polled yet or waiting for the lock), "parked" (captured and waiting
  \* for the lock, which only the bug CaptureFirst does), "run" (its batch
  \* holds the lock, next call to make), "await" (a browser call in
  \* flight), or "killed" (dropped with a browser call in flight, which only
  \* the bug NoKeepAlive allows).
  pc,
  \* Each slot's batch: the calls it has not made yet, in order.
  todo,
  \* Each slot's writes that went through: the version written per file, 0
  \* for none (`written`).
  wrote,
  \* Each slot's captured owed names (`deletes`), forgotten at settle.
  dels,
  \* Each slot's batch will end with an error: a write failed, and the bug
  \* DeleteAfterFailedWrite went on to the removals. FALSE otherwise.
  doomed,
  \* The browser call each slot has in flight, or NoOp.
  inflight,
  \* OPFS: the version each file's entry holds, 0 for no entry.
  disk,
  \* A ghost: the newest MANIFEST version OPFS ever held.
  peak,
  \* How many more browser calls may fail.
  faults,
  \* A ghost: some call returned an error to its caller.
  failed

\* All of them, for "nothing else changes".
vars == <<stage, pers, deleted, holder, pc, todo, wrote, dels, doomed, inflight, disk, peak,
          faults, failed>>

\* ---------------------------------------------------------------------------
\* The mirror's files
\* ---------------------------------------------------------------------------

\* Whether the mirror holds file f now (MirrorFs::files).
InMirror(f) ==
  \* The MANIFEST always.
  CASE f = "M" -> TRUE
    \* T1 until the compaction removes it.
    [] f = "T1" -> stage <= 1
    \* T2 from the second flush until the compaction removes it.
    [] f = "T2" -> stage = 1
    \* T3 from the compaction on.
    [] OTHER -> stage = 2

\* The MANIFEST's version in the mirror: one edit per engine step.
MVer == stage + 1

\* Entry::version of file f, 0 for a file with no entry. A table is written
\* once and never changed, so it has one version.
Ver(f) == IF ~InMirror(f) THEN 0 ELSE IF f = "M" THEN MVer ELSE 1

\* Entry::is_dirty: the mirror holds a version no batch settled.
Dirty(f) == InMirror(f) /\ Ver(f) > pers[f]

\* The write of each dirty file of fs, in the order of fs, at its version now.
RECURSIVE WritesOf(_)
\* Defined here.
WritesOf(fs) ==
  \* No file left to look at.
  IF fs = <<>> THEN <<>>
  \* The first file's write if it is dirty, then the others'.
  ELSE (IF Dirty(Head(fs)) THEN <<[k |-> "w", f |-> Head(fs), v |-> Ver(Head(fs))]>> ELSE <<>>)
       \o WritesOf(Tail(fs))

\* The removal of each owed name of fs, in the order of fs.
RECURSIVE RemovalsOf(_)
\* Defined here.
RemovalsOf(fs) ==
  \* No name left to look at.
  IF fs = <<>> THEN <<>>
  \* The first name's removal if it is owed, then the others'.
  ELSE (IF Head(fs) \in deleted THEN <<[k |-> "r", f |-> Head(fs), v |-> 0]>> ELSE <<>>)
       \o RemovalsOf(Tail(fs))

\* The batch a call captures now: its writes, tables first and the MANIFEST
\* last, then its removals (take_persist_batch, then the two loops of
\* persist_batch). The bugs ManifestFirst and DeleteFirst change the order.
Batch ==
  \* The bug: the MANIFEST before the tables.
  CASE Mutant = "ManifestFirst" -> WritesOf(OrderManifestFirst) \o RemovalsOf(RemovalOrder)
    \* The bug: the removals before the writes.
    [] Mutant = "DeleteFirst" -> RemovalsOf(RemovalOrder) \o WritesOf(Order)
    \* The code.
    [] OTHER -> WritesOf(Order) \o RemovalsOf(RemovalOrder)

\* What a browser call can come back with: "ok"; "notfound" for a removal of
\* a name OPFS has no entry for (NotFoundError); or "fail" (any other error)
\* while the failure budget lasts.
Results(op) ==
  \* What the call itself finds.
  {IF op.k = "r" /\ disk[op.f] = 0 THEN "notfound" ELSE "ok"}
  \* Or a failure, while any is left.
  \cup (IF faults > 0 THEN {"fail"} ELSE {})

\* The start: the database opened and flushed once, nothing persisted yet.
Init ==
  \* T1 and MANIFEST 1 in memory.
  /\ stage = 0
  \* Nothing settled.
  /\ pers = [f \in Files |-> 0]
  \* Nothing owed a removal.
  /\ deleted = {}
  \* The lock is free.
  /\ holder = 0
  \* No call made.
  /\ pc = [t \in Tasks |-> "idle"]
  \* No batch.
  /\ todo = [t \in Tasks |-> <<>>]
  \* Nothing written.
  /\ wrote = [t \in Tasks |-> NoWrites]
  \* No names captured.
  /\ dels = [t \in Tasks |-> {}]
  \* No batch doomed.
  /\ doomed = [t \in Tasks |-> FALSE]
  \* Nothing in the browser.
  /\ inflight = [t \in Tasks |-> NoOp]
  \* OPFS holds nothing of this database.
  /\ disk = [f \in Files |-> 0]
  \* It never held a MANIFEST.
  /\ peak = 0
  \* The whole failure budget is left.
  /\ faults = MaxFaults
  \* No call failed.
  /\ failed = FALSE

\* ---------------------------------------------------------------------------
\* A persist call
\* ---------------------------------------------------------------------------

\* A slot's batch is let go: no call left, nothing written or captured.
Clear(t) ==
  \* No call left.
  /\ todo' = [todo EXCEPT ![t] = <<>>]
  \* Nothing written.
  /\ wrote' = [wrote EXCEPT ![t] = NoWrites]
  \* No names captured.
  /\ dels' = [dels EXCEPT ![t] = {}]
  \* Not doomed.
  /\ doomed' = [doomed EXCEPT ![t] = FALSE]

\* A slot captures its batch (take_persist_batch, 282-298).
Capture(t) ==
  \* The calls, in order.
  /\ todo' = [todo EXCEPT ![t] = Batch]
  \* Nothing written yet.
  /\ wrote' = [wrote EXCEPT ![t] = NoWrites]
  \* The owed names it forgets at settle.
  /\ dels' = [dels EXCEPT ![t] = deleted]

\* The app calls OpfsEnv::persist (mod.rs:322); MirrorFs::persist starts the
\* batch as a promise of its own (future_to_promise, 319).
Start(t) ==
  \* The slot has no call.
  /\ pc[t] = "idle"
  \* The caller contract: the app calls persist again after a call that
  \* failed. The bug NoRetry is a caller that stops once one failed.
  /\ ~(Mutant = "NoRetry" /\ failed)
  \* Called: the batch's first poll comes later, as a microtask.
  /\ pc' = [pc EXCEPT ![t] = "wait"]
  \* Nothing else changes.
  /\ UNCHANGED <<stage, pers, deleted, holder, todo, wrote, dels, doomed, inflight, disk, peak,
                 faults, failed>>

\* The batch takes the lock (`persist_lock.lock().await`, 328) and captures
\* (329), with no await between the two.
Acquire(t) ==
  \* It waits for the lock, or (the bug CaptureFirst) captured and parked.
  /\ pc[t] \in {"wait", "parked"}
  \* The lock is free; the bug NoLock has none.
  /\ (holder = 0 \/ Mutant = "NoLock")
  \* Taken.
  /\ holder' = IF Mutant = "NoLock" THEN holder ELSE t
  \* The batch runs.
  /\ pc' = [pc EXCEPT ![t] = "run"]
  \* It captures now, unless it captured before it parked.
  /\ IF pc[t] = "parked" THEN UNCHANGED <<todo, wrote, dels>> ELSE Capture(t)
  \* Nothing else changes.
  /\ UNCHANGED <<stage, pers, deleted, doomed, inflight, disk, peak, faults, failed>>

\* The bug CaptureFirst: the batch's first poll captures, then asks for the
\* lock. With the lock free it takes it at once, which is Acquire; with the
\* lock held it parks holding the batch it captured.
CaptureEarly(t) ==
  \* Only the bug does this.
  /\ Mutant = "CaptureFirst"
  \* Its first poll.
  /\ pc[t] = "wait"
  \* Another batch holds the lock.
  /\ holder # 0
  \* It captures,
  /\ Capture(t)
  \* and parks on the lock.
  /\ pc' = [pc EXCEPT ![t] = "parked"]
  \* Nothing else changes.
  /\ UNCHANGED <<stage, pers, deleted, holder, doomed, inflight, disk, peak, faults, failed>>

\* The batch makes its next browser call and awaits it: a write's close
\* (js.rs:263-264, from 338) or a removeEntry (js.rs:101-104, from 344).
Issue(t) ==
  \* The batch runs.
  /\ pc[t] = "run"
  \* A call is left.
  /\ todo[t] # <<>>
  \* The browser has it now.
  /\ inflight' = [inflight EXCEPT ![t] = Head(todo[t])]
  \* One call fewer left.
  /\ todo' = [todo EXCEPT ![t] = Tail(todo[t])]
  \* The batch awaits it.
  /\ pc' = [pc EXCEPT ![t] = "await"]
  \* Nothing else changes.
  /\ UNCHANGED <<stage, pers, deleted, holder, wrote, dels, doomed, disk, peak, faults, failed>>

\* Whether a call's result ends the batch with an error, before settle.
Ends(op, r) ==
  \* A failed write (the `?` at 338), unless the bug goes on.
  \/ r = "fail" /\ op.k = "w" /\ Mutant # "DeleteAfterFailedWrite"
  \* A failed removal that is not NotFoundError (347), unless the bug
  \* swallows it.
  \/ r = "fail" /\ op.k = "r" /\ Mutant # "SwallowRemoval"
  \* The bug NotFoundFails: NotFoundError ends it too.
  \/ r = "notfound" /\ Mutant = "NotFoundFails"

\* The bug DeleteAfterFailedWrite: a failed write drops the writes left and
\* goes on to the removals.
Skips(op, r) == r = "fail" /\ op.k = "w" /\ Mutant = "DeleteAfterFailedWrite"

\* The batch resumes after its call op came back with r.
Resume(t, op, r) ==
  \* The call failed: the batch returns the error.
  CASE Ends(op, r) ->
         \* It lets go of the lock: the guard is dropped as it returns.
         /\ holder' = IF holder = t THEN 0 ELSE holder
         \* The slot has no call.
         /\ pc' = [pc EXCEPT ![t] = "idle"]
         \* The batch is let go, nothing settled.
         /\ Clear(t)
         \* The caller is told an error.
         /\ failed' = TRUE
    \* The bug: on to the removals, the error kept for the end.
    [] Skips(op, r) ->
         \* Only the removals are left.
         /\ todo' = [todo EXCEPT ![t] = SelectSeq(todo[t], LAMBDA o : o.k = "r")]
         \* It will end with the error.
         /\ doomed' = [doomed EXCEPT ![t] = TRUE]
         \* It goes on.
         /\ pc' = [pc EXCEPT ![t] = "run"]
         \* Nothing else changes.
         /\ UNCHANGED <<holder, wrote, dels, failed>>
    \* The call worked, or counts as having worked: on to the next.
    [] OTHER ->
         \* A write that went through is recorded (`written.push`, 339).
         /\ wrote' = IF op.k = "w" /\ r = "ok" THEN [wrote EXCEPT ![t][op.f] = op.v] ELSE wrote
         \* It goes on.
         /\ pc' = [pc EXCEPT ![t] = "run"]
         \* Nothing else changes.
         /\ UNCHANGED <<holder, todo, dels, doomed, failed>>

\* The browser finishes the call a slot has in flight, whether or not the
\* batch that made it is still there, and the batch, if it is, resumes.
Land(t) ==
  \* A call is in flight.
  /\ inflight[t] # NoOp
  \* It comes back with some result r.
  /\ \E r \in Results(inflight[t]) :
       \* The call.
       LET op == inflight[t]
       \* What it does to OPFS, and what follows.
       IN /\ disk' = IF r = "ok" THEN [disk EXCEPT ![op.f] = op.v] ELSE disk
          \* The newest MANIFEST version OPFS ever held.
          /\ peak' = IF r = "ok" /\ op.f = "M" THEN Max(peak, op.v) ELSE peak
          \* A failure uses up one of the budget.
          /\ faults' = IF r = "fail" THEN faults - 1 ELSE faults
          \* Nothing in flight for this slot now.
          /\ inflight' = [inflight EXCEPT ![t] = NoOp]
          \* What the slot does.
          /\ IF pc[t] = "killed"
                \* Its batch was dropped: nobody resumes, the slot is free.
                THEN /\ pc' = [pc EXCEPT ![t] = "idle"]
                     \* Nothing else changes.
                     /\ UNCHANGED <<holder, todo, wrote, dels, doomed, failed>>
                \* The batch resumes.
                ELSE Resume(t, op, r)
  \* The engine and the mirror's records stay.
  /\ UNCHANGED <<stage, pers, deleted>>

\* Every call of the batch came back: settle (352; 302-311) and return Ok
\* (353), letting go of the lock.
Finish(t) ==
  \* The batch runs,
  /\ pc[t] = "run"
  \* and no call is left.
  /\ todo[t] = <<>>
  \* What the batch leaves.
  /\ IF doomed[t]
        \* The bug DeleteAfterFailedWrite returns the kept error, no settle.
        THEN /\ failed' = TRUE
             \* Nothing settled.
             /\ UNCHANGED <<pers, deleted>>
        \* Settle: each version written counts as persisted, for a file the
        \* mirror still holds (304-305), and the captured names are owed no
        \* more (309).
        ELSE /\ pers' = [f \in Files |-> IF InMirror(f) THEN Max(pers[f], wrote[t][f]) ELSE pers[f]]
             \* The captured names.
             /\ deleted' = deleted \ dels[t]
             \* The caller is told Ok.
             /\ UNCHANGED failed
  \* The lock is let go.
  /\ holder' = IF holder = t THEN 0 ELSE holder
  \* The slot has no call.
  /\ pc' = [pc EXCEPT ![t] = "idle"]
  \* The batch is let go.
  /\ Clear(t)
  \* Nothing else changes.
  /\ UNCHANGED <<stage, inflight, disk, peak, faults>>

\* The caller drops the future it awaits (the JsFuture of 323).
Cancel(t) ==
  \* Its call has not ended.
  /\ pc[t] \in {"wait", "parked", "run", "await"}
  \* What that does.
  /\ IF Mutant = "NoKeepAlive"
        \* The bug: the batch is dropped with the future, and its guard.
        THEN /\ holder' = IF holder = t THEN 0 ELSE holder
             \* A call already in the browser goes on without it.
             /\ pc' = [pc EXCEPT ![t] = IF inflight[t] # NoOp THEN "killed" ELSE "idle"]
             \* The batch is let go.
             /\ Clear(t)
             \* Nothing else changes.
             /\ UNCHANGED <<stage, pers, deleted, inflight, disk, peak, faults, failed>>
        \* The code: the promise keeps the batch going, the lock held. Nothing
        \* this model holds changes.
        ELSE UNCHANGED vars

\* ---------------------------------------------------------------------------
\* The engine, between two awaits
\* ---------------------------------------------------------------------------

\* The second flush: table T2, and the edit it appends to the MANIFEST.
Flush ==
  \* After the open.
  /\ stage = 0
  \* T2 and MANIFEST 2 are in the mirror, both dirty.
  /\ stage' = 1
  \* Nothing else changes.
  /\ UNCHANGED <<pers, deleted, holder, pc, todo, wrote, dels, doomed, inflight, disk, peak,
                 faults, failed>>

\* A compaction merges T1 and T2 into T3: table T3, its edit to the MANIFEST,
\* and then T1 and T2 removed from the mirror.
Compact ==
  \* After the second flush.
  /\ stage = 1
  \* T3 and MANIFEST 3 are in the mirror, T1 and T2 are not.
  /\ stage' = 2
  \* Their names are owed a removal (436).
  /\ deleted' = deleted \cup {"T1", "T2"}
  \* Their entries are gone, and with them what they had settled.
  /\ pers' = [pers EXCEPT !["T1"] = 0, !["T2"] = 0]
  \* Nothing else changes.
  /\ UNCHANGED <<holder, pc, todo, wrote, dels, doomed, inflight, disk, peak, faults, failed>>

\* ---------------------------------------------------------------------------
\* The whole system
\* ---------------------------------------------------------------------------

\* What a slot, and the browser for it, do: they keep doing it.
Step(t) ==
  \* A call starts,
  \/ Start(t)
  \* takes the lock,
  \/ Acquire(t)
  \* or, in the bug CaptureFirst, captures and parks on it,
  \/ CaptureEarly(t)
  \* makes a browser call,
  \/ Issue(t)
  \* the browser finishes it,
  \/ Land(t)
  \* or the batch settles.
  \/ Finish(t)

\* Any step of anyone.
Next ==
  \* A slot's step.
  \/ \E t \in Tasks : Step(t)
  \* A caller drops its future.
  \/ \E t \in Tasks : Cancel(t)
  \* The engine flushes.
  \/ Flush
  \* The engine compacts.
  \/ Compact

\* Each slot and the browser keep going: the app keeps calling persist (the
\* caller contract in OpfsEnv::persist's doc, mod.rs:310-321), and the
\* browser finishes every call. The engine and the callers' drops may or may
\* not happen.
Spec == Init /\ [][Next]_vars /\ \A t \in Tasks : WF_vars(Step(t))

\* ---------------------------------------------------------------------------
\* The promises
\* ---------------------------------------------------------------------------

\* Every variable keeps its type.
TypeOK ==
  \* The engine's progress.
  /\ stage \in 0..2
  \* Settled versions.
  /\ pers \in [Files -> 0..3]
  \* Owed names.
  /\ deleted \subseteq Tables
  \* The lock.
  /\ holder \in Tasks \cup {0}
  \* Where each call is.
  /\ pc \in [Tasks -> {"idle", "wait", "parked", "run", "await", "killed"}]
  \* Each batch's calls left are calls.
  /\ \A t \in Tasks : \A i \in DOMAIN todo[t] : todo[t][i] \in Ops
  \* Writes recorded.
  /\ wrote \in [Tasks -> [Files -> 0..3]]
  \* Captured names.
  /\ dels \in [Tasks -> SUBSET Tables]
  \* Doomed batches.
  /\ doomed \in [Tasks -> BOOLEAN]
  \* Calls in the browser.
  /\ inflight \in [Tasks -> Ops \cup {NoOp}]
  \* OPFS.
  /\ disk \in [Files -> 0..3]
  \* The ghost of the newest MANIFEST.
  /\ peak \in 0..3
  \* The budget.
  /\ faults \in 0..MaxFaults
  \* The ghost of a failed call.
  /\ failed \in BOOLEAN

\* Promise: an older MANIFEST never replaces a newer one in OPFS: what OPFS
\* holds is the newest it ever held.
INV_ManifestMonotone == disk["M"] = peak

\* Promise: a version the mirror counts as persisted is in OPFS, or a newer
\* one is, so no file the mirror calls clean is stale in OPFS.
INV_CleanIsStored ==
  \* For every file the mirror holds that a batch settled,
  \A f \in Files :
    \* OPFS holds that version or a newer one.
    InMirror(f) /\ pers[f] > 0 => disk[f] >= pers[f]

\* Promise: while a batch runs, no browser call of another batch is in
\* flight, so a dropped call's batch finished before the next one started.
INV_SerialBatches ==
  \* For every slot whose batch runs,
  \A t \in Tasks :
    \* every other slot has nothing in the browser.
    pc[t] \in {"run", "await"} => \A u \in Tasks \ {t} : inflight[u] = NoOp

\* Promise: an OPFS entry for a name the mirror no longer holds is still owed
\* a removal, so a removal that failed is made again by a later call.
INV_RemovalOwed ==
  \* For every table,
  \A f \in Tables :
    \* gone from the mirror but still in OPFS means owed.
    ~InMirror(f) /\ disk[f] # 0 => f \in deleted

\* Promise: every table OPFS's MANIFEST names is in OPFS, at every moment, so
\* a tab closed or a call failed at any point leaves a database that opens.
INV_ManifestNamesStored ==
  \* For every table the MANIFEST in OPFS names,
  \A x \in Names(disk["M"]) :
    \* OPFS has an entry for it.
    disk[x] # 0

\* The mirror and OPFS agree: nothing dirty, nothing owed, and OPFS holds
\* exactly the mirror's versions and nothing else.
Caught ==
  \* Nothing owed a removal.
  /\ deleted = {}
  \* Every file the mirror holds is settled at its version.
  /\ \A f \in Files : InMirror(f) => pers[f] = Ver(f)
  \* OPFS holds every file at the mirror's version, and no other file.
  /\ \A f \in Files : disk[f] = Ver(f)

\* Promise, over whole runs: while the app keeps calling persist, the mirror
\* and OPFS end up the same, and stay so.
LIVE_CatchesUp == <>[]Caught

\* A witness, not a promise: no call waits while another's write of an older
\* MANIFEST is in flight. It must break in the GREEN run, which shows the run
\* reaches the race the lock is there for.
NeverWaitsBehindOlder ==
  \* No waiting call beside a MANIFEST write older than the mirror's.
  ~\E t, u \in Tasks : pc[u] = "wait" /\ inflight[t].f = "M" /\ inflight[t].v < MVer

====
