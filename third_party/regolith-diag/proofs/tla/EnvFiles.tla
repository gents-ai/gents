---- MODULE EnvFiles ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* regolith reads and writes its files through an Env. Two Envs keep files
\* in memory (MemEnv, and the OPFS mirror in a browser tab), and the real
\* one reads files from the disk. Many threads read at once: get, scans,
\* compaction. The rule every Env must keep:
\*
\*     a read sees the bytes of every write that finished before it began,
\*     at the offset it asked for.
\*
\* PART 1, the disk: a shared cursor. An open file has one "cursor"
\* position. Reading with seek-then-read is two steps on that one cursor:
\* reader A seeks to 0, reader B seeks to 100, reader A reads... at 100. A
\* got B's bytes. A positional read (pread, StdReadFile in
\* src/env/std_env.rs) names its offset in the one call, so there is no
\* shared position to race on.
\*
\* PART 2, a file in memory (MemFile, src/env/mem_file.rs). The file is a
\* buffer (a chain of segments an append lengthens in place,
\* src/env/mem_file/extent.rs) and a published length L; readers read below
\* L with no lock. An append writes ABOVE L, linking more room first if it
\* runs out, under a WRITER flag it took with a compare-and-swap (CAS), and
\* then publishes L+n with a second CAS. Anything else (an overwrite, a
\* shrink, two appends at once) builds a NEW buffer from the old one's first
\* L bytes plus its own, FREEZES the old buffer with a CAS so no append can
\* publish there any more, and swaps the new buffer in with a CAS on the
\* file's pointer.
\*
\* Tiny example of why the freeze matters. Writer A holds WRITER on buffer
\* E and is copying its record above L. Writer B gives up on E (it is busy)
\* and copies E's first L bytes into E2, plus its own record, and swaps E2
\* in. If B did not freeze E, A now publishes on E, an old buffer nobody
\* reads any more: A's append "finished", but no later read can see it.
\* With the freeze, A's publishing CAS fails, and A appends again on E2.
\*
\* PART 3, a table renamed while its handles reopen it. Under
\* max_open_files a table's file is closed while it is not read, and
\* reopened by name (PathEntry::reopen, src/env/open_file_limit/mod.rs).
\* A compaction that removes a table some reader still holds renames the
\* file aside instead (Shared::relocate): it first records both names (the
\* old one as "previous"), then asks the Env to rename, then records only
\* the new one. A reopen loads the names and tries "previous", then
\* "current". Two things must hold for that to always find the file:
\*   - the Env's rename puts the file under the new name BEFORE it takes it
\*     from the old one (MemEnv::rename, and the OPFS mirror and pool), so
\*     at every instant one of the two names holds it. MemEnv's rename first
\*     marks the old name as leaving, a write that takes the file from no
\*     name and so changes nothing a reopen sees; it is left out here, and
\*     EnvRename.tla checks what it is for: a removal or a second rename
\*     racing the move;
\*   - a reopen that loaded the names before the whole move ran (it knows
\*     only the old name, and the file is already under the new one) loads
\*     the names again when its open finds nothing, and tries again.
\* Tiny example. Thread 1 loads the names: "A". Thread 2 moves the table
\* to "B" from start to end. Thread 1 opens "A": nothing there. It loads
\* the names again, sees "B", and opens "B".
\*
\* What this model checks cannot happen:
\*   - a disk read returns bytes from another offset (ReadsOwnBytes);
\*   - a memory read misses a write that finished before it began
\*     (ReadSeesFinished), or the current buffer misses one
\*     (CurrentHoldsFinished);
\*   - a record lands twice (AtMostOnce);
\*   - a reopen finds the file under none of its names while it is being
\*     renamed (OpenFindsFile).
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   cursor, pgot     the shared position of a seek-then-read file, and
\*                    what each disk reader got
\*   cur              MemFile::current, the buffer readers load
\*   buf, len         an Extent's bytes (published and pending) and its
\*                    published length (the LEN bits of Extent::state)
\*   wbit, frozen     the WRITER and FROZEN bits of Extent::state
\*   finished         the appends that returned
\*   wpc.. / rpc..    each writer's and reader's step and what it holds
\*   mph              how far the move has got (PathEntry::names and the
\*                    Env's map: which names hold the file)
\*   opc, over        each reopen's step, and which recorded names it loaded
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Byte counts: a record is one slot of a sequence; an append of n
\*     bytes is the same protocol with n slots.
\*   - Overwrites and shrinks: they always take the copy path, which is
\*     the path modelled here for appends too.
\*   - The room: an append in place that runs out of it links more
\*     (src/env/mem_file/extent.rs) while it holds WRITER, above the
\*     published length, which is the in-place write modelled here; and
\*     here a writer may take either path at any time. MemFileExtents.tla
\*     models the bytes: the chain of segments, the zeroing of room a write
\*     exposes, and the reclamation of a replaced extent.
\*   - An append in place whose room cannot be allocated: it gives WRITER
\*     back with a CAS from the word it claimed (a no-op once a copy froze
\*     the buffer) and fails before it writes a byte (MemFile::in_place).
\*     Like a publish lost to a freeze, it publishes nothing and does not
\*     return as finished, so no rule here can see it; the claim it held
\*     only sent other writers to the copy path meanwhile, which they may
\*     take at any time here anyway.
\*   - Memory ordering: the CAS steps are atomic here; the code's Acquire
\*     and Release make them so.
\*   - An append at one length only (MemFile::append_at, which the
\*     in-memory positional log of src/env/mem_log.rs is built on): the
\*     append modelled here with one more condition, that the file is as
\*     long as the caller says, read from the same state word the append
\*     then moves. An append the condition turns away changes nothing, so
\*     it is a step that is not taken.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one
\* invariant it names):
\*   MC_EnvFiles_Green               two disk readers, two appenders, one
\*                                   memory reader.
\*   MC_EnvFiles_Green_ThreeWriters  three appenders, so one publishes in
\*                                   place between another's build and a
\*                                   third's freeze.
\*   MC_EnvFiles_Red_SharedCursor    disk reads seek then read on one shared
\*                                   cursor: ReadsOwnBytes breaks.
\*   MC_EnvFiles_Red_NoFreeze        a copy swaps without freezing the old
\*                                   buffer: CurrentHoldsFinished breaks.
\*   MC_EnvFiles_Red_StaleFrozen     a copy skips its freeze because the
\*                                   buffer is frozen NOW, though it was not
\*                                   at its build: CurrentHoldsFinished
\*                                   breaks.
\*   MC_EnvFiles_Green_Rename        two reopens racing one move. Every rule
\*                                   holds.
\*   MC_EnvFiles_Red_RemoveFirst     the Env's rename takes the old name
\*                                   before it adds the new one: OpenFindsFile
\*                                   breaks.
\*   MC_EnvFiles_Red_NoReload        a reopen that found nothing gives up
\*                                   without loading the names again:
\*                                   OpenFindsFile breaks.
\*
\* Lean, proofs/lean/Regolith/EnvFiles.lean, proves the first four rules
\* for every number of readers, writers and steps;
\* proofs/lean/Regolith/EnvFilesRename.lean proves OpenFindsFile for every
\* number of reopens and every order of steps.

\* We use numbers, sequences and finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* Threads reading a file on disk, each at its own offset.
  DiskReaders,
  \* [DiskReaders -> Nat]: the offset each disk reader reads.
  At,
  \* Threads appending to the in-memory file; writer w appends record w.
  Writers,
  \* Threads reading the whole in-memory file once.
  MemReaders,
  \* How many buffers may ever exist (a bound for TLC).
  MaxExt,
  \* Threads reopening a table while it is renamed aside (Part 3).
  Openers,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The bug names this model knows.
ASSUME Mutant \in {"none", "SharedCursor", "NoFreeze", "StaleFrozen",
                   \* and Part 3's: a rename that removes first, a reopen
                   \* that never loads the names again.
                   "RemoveFirst", "NoReload"}
\* Every disk reader reads one offset.
ASSUME At \in [DiskReaders -> Nat]

\* For the configurations: disk reader 1 reads offset 0, reader 2 offset 1.
AtTwo == [r \in {1, 2} |-> r - 1]
\* For the configurations: no disk reader.
NoDisk == [r \in {} |-> 0]

\* The bytes of the file on disk at offset o: a table file never changes,
\* so each offset holds one known value.
Data(o) == 100 + o

\* The buffer numbers.
Exts == 1..MaxExt

\* The set of values in a sequence.
Range(s) == {s[i] : i \in DOMAIN s}

\* The state that changes from step to step.
VARIABLES
  \* Part 1: the shared cursor of a seek-then-read file.
  cursor,
  \* [DiskReaders -> {"start","sought","done"}]: each disk reader's step.
  ppc,
  \* [DiskReaders -> Nat]: what each disk reader got (0: nothing yet).
  pgot,
  \* Part 2: the buffer readers load now.
  cur,
  \* [Exts -> Seq(Writers)]: each buffer's records, published or pending.
  buf,
  \* [Exts -> Nat]: each buffer's published length.
  len,
  \* [Exts -> BOOLEAN]: each buffer's WRITER bit.
  wbit,
  \* [Exts -> BOOLEAN]: each buffer's FROZEN bit.
  frozen,
  \* The next buffer number.
  nextExt,
  \* The appends that returned.
  finished,
  \* [Writers -> step name]: each writer's step.
  wpc,
  \* [Writers -> Exts]: the buffer a writer works on.
  wext,
  \* [Writers -> Nat]: the length a writer saw.
  wlen,
  \* [Writers -> [bit, frz]]: the WRITER and FROZEN bits a copier saw when
  \* it built (the state word its freeze CAS expects).
  wsaw,
  \* [Writers -> Seq(Writers)]: the new buffer a copier built.
  wnew,
  \* [MemReaders -> {"start","begun","loaded","lenread","done"}].
  rpc,
  \* [MemReaders -> SUBSET Writers]: appends finished when the read began.
  rfin,
  \* [MemReaders -> Exts]: the buffer a reader loaded.
  rext,
  \* [MemReaders -> Nat]: the length it loaded.
  rlen,
  \* [MemReaders -> Seq(Writers)]: what it read.
  rgot,
  \* Part 3: how far the move has got, 0 (not begun) to 4 (settled).
  mph,
  \* [Openers -> step name]: each reopen's step.
  opc,
  \* [Openers -> 0..2]: which recorded names each reopen loaded.
  over

\* All the variables, for "nothing else changes".
\* The disk part's variables.
diskVars == <<cursor, ppc, pgot>>
\* The in-memory part's variables.
memVars == <<cur, buf, len, wbit, frozen, nextExt, finished, wpc, wext, wlen,
             wsaw, wnew, rpc, rfin, rext, rlen, rgot>>
\* The rename part's variables.
nameVars == <<mph, opc, over>>
\* All three together.
vars == <<diskVars, memVars, nameVars>>

\* Every variable holds the kind of value it should.
TypeOK ==
  \* The cursor is an offset.
  /\ cursor \in Nat
  \* Each disk reader is at one step.
  /\ ppc \in [DiskReaders -> {"start", "sought", "done"}]
  \* The current buffer is a buffer number.
  /\ cur \in Exts
  \* Lengths are numbers and bits are booleans.
  /\ len \in [Exts -> Nat]
  \* Each buffer's WRITER bit is set or not.
  /\ wbit \in [Exts -> BOOLEAN]
  \* Each buffer is frozen or not.
  /\ frozen \in [Exts -> BOOLEAN]
  \* Finished appends are writers' records.
  /\ finished \subseteq Writers
  \* Each writer and memory reader is at one step.
  /\ wpc \in [Writers -> {"start", "claimed", "written", "built", "frozen", "done"}]
  \* Each memory reader is at one of its steps.
  /\ rpc \in [MemReaders -> {"start", "begun", "loaded", "lenread", "done"}]
  \* The move is in one of its five phases.
  /\ mph \in 0..4
  \* Each reopen is at one of its six steps.
  /\ opc \in [Openers -> {"load", "prev", "cur", "reload", "found", "lost"}]
  \* Each reopen holds one of the three recorded names.
  /\ over \in [Openers -> 0..2]

\* The start: an empty file in buffer 1, nobody has done anything.
Init ==
  \* The cursor is at 0.
  /\ cursor = 0
  \* Disk readers have not read.
  /\ ppc = [r \in DiskReaders |-> "start"]
  \* They have got no bytes.
  /\ pgot = [r \in DiskReaders |-> 0]
  \* Readers load buffer 1.
  /\ cur = 1
  \* Every buffer is empty.
  /\ buf = [e \in Exts |-> <<>>]
  \* Every published length is 0.
  /\ len = [e \in Exts |-> 0]
  \* No WRITER bit, nothing frozen.
  /\ wbit = [e \in Exts |-> FALSE]
  \* No buffer is frozen.
  /\ frozen = [e \in Exts |-> FALSE]
  \* The next new buffer is number 2.
  /\ nextExt = 2
  \* No append has returned.
  /\ finished = {}
  \* Every writer is about to append.
  /\ wpc = [w \in Writers |-> "start"]
  \* Every writer's remembered buffer is buffer 1, not used yet;
  /\ wext = [w \in Writers |-> 1]
  \* its remembered length is 0;
  /\ wlen = [w \in Writers |-> 0]
  \* it saw neither bit;
  /\ wsaw = [w \in Writers |-> [bit |-> FALSE, frz |-> FALSE]]
  \* and it has built nothing.
  /\ wnew = [w \in Writers |-> <<>>]
  \* Every memory reader is about to read.
  /\ rpc = [r \in MemReaders |-> "start"]
  \* No memory reader has noted which appends returned;
  /\ rfin = [r \in MemReaders |-> {}]
  \* each holds buffer 1, not used yet;
  /\ rext = [r \in MemReaders |-> 1]
  \* has read the length 0;
  /\ rlen = [r \in MemReaders |-> 0]
  \* and has copied nothing.
  /\ rgot = [r \in MemReaders |-> <<>>]
  \* The move has not begun: the file is under "A", and the names say "A".
  /\ mph = 0
  \* Every reopen is about to load the names.
  /\ opc = [o \in Openers |-> "load"]
  \* Each reopen holds the names from before the move.
  /\ over = [o \in Openers |-> 0]

\* ---------------------------------------------------------------------------
\* Part 1: disk reads.
\* ---------------------------------------------------------------------------

\* A positional read: one call names the offset and returns its bytes
\* (pread). The bug SharedCursor never takes this step.
PRead(r) ==
  \* The real code.
  /\ Mutant # "SharedCursor"
  \* The reader has not read.
  /\ ppc[r] = "start"
  \* It gets the bytes at its own offset, in one step.
  /\ pgot' = [pgot EXCEPT ![r] = Data(At[r])]
  \* The read is done.
  /\ ppc' = [ppc EXCEPT ![r] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<cursor, memVars>>

\* Bug SharedCursor, step 1: move the file's one cursor to my offset.
Seek(r) ==
  \* Only with the bug.
  /\ Mutant = "SharedCursor"
  \* The reader has not read.
  /\ ppc[r] = "start"
  \* The shared cursor goes to its offset.
  /\ cursor' = At[r]
  \* The reader has sought.
  /\ ppc' = [ppc EXCEPT ![r] = "sought"]
  \* Nothing else changes.
  /\ UNCHANGED <<pgot, memVars>>

\* Bug SharedCursor, step 2: read at the cursor, wherever it is now.
ReadAtCursor(r) ==
  \* The reader sought.
  /\ ppc[r] = "sought"
  \* It gets the bytes at the cursor, which another reader may have moved.
  /\ pgot' = [pgot EXCEPT ![r] = Data(cursor)]
  \* The read is done.
  /\ ppc' = [ppc EXCEPT ![r] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<cursor, memVars>>

\* ---------------------------------------------------------------------------
\* Part 2: the in-memory file.
\* ---------------------------------------------------------------------------

\* An append in place, step 1: take WRITER on the current buffer with a CAS
\* that needs no WRITER and no FROZEN (MemFile::in_place).
InPlaceClaim(w) ==
  \* The writer is about to append.
  /\ wpc[w] = "start"
  \* The current buffer is free to append to.
  /\ ~wbit[cur]
  \* (It is not frozen.)
  /\ ~frozen[cur]
  \* The CAS sets WRITER.
  /\ wbit' = [wbit EXCEPT ![cur] = TRUE]
  \* The writer remembers the buffer and its length.
  /\ wext' = [wext EXCEPT ![w] = cur]
  \* (Its published length.)
  /\ wlen' = [wlen EXCEPT ![w] = len[cur]]
  \* The writer has claimed the buffer.
  /\ wpc' = [wpc EXCEPT ![w] = "claimed"]
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, cur, buf, len, frozen, nextExt, finished, wsaw, wnew,
                 rpc, rfin, rext, rlen, rgot>>

\* An append in place, step 2: write the record just above the published
\* length. No reader reads there.
InPlaceWrite(w) ==
  \* The writer holds WRITER.
  /\ wpc[w] = "claimed"
  \* Its record goes right after the published records.
  /\ buf' = [buf EXCEPT ![wext[w]] = SubSeq(buf[wext[w]], 1, wlen[w]) \o <<w>>]
  \* The writer has written its record.
  /\ wpc' = [wpc EXCEPT ![w] = "written"]
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, cur, len, wbit, frozen, nextExt, finished, wext, wlen,
                 wsaw, wnew, rpc, rfin, rext, rlen, rgot>>

\* An append in place, step 3: publish the new length with a CAS that
\* clears WRITER. It fails if a copier froze the buffer meanwhile; then the
\* writer appends again from the start.
InPlacePublish(w) ==
  \* The writer wrote its record.
  /\ wpc[w] = "written"
  \* e is the buffer the writer claimed.
  /\ LET e == wext[w] IN
       \* Not frozen: the CAS publishes, and the append has finished.
       IF ~frozen[e]
       \* The published length grows by one,
       THEN /\ len' = [len EXCEPT ![e] = wlen[w] + 1]
            \* the WRITER bit is cleared,
            /\ wbit' = [wbit EXCEPT ![e] = FALSE]
            \* the append has returned,
            /\ finished' = finished \cup {w}
            \* and the writer is done.
            /\ wpc' = [wpc EXCEPT ![w] = "done"]
       \* Frozen: the CAS fails; the buffer is dead and the writer retries.
       ELSE /\ wpc' = [wpc EXCEPT ![w] = "start"]
            \* Nothing is published.
            /\ UNCHANGED <<len, wbit, finished>>
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, cur, buf, frozen, nextExt, wext, wlen, wsaw, wnew,
                 rpc, rfin, rext, rlen, rgot>>

\* A copy, step 1: build a new buffer from the current one's published
\* records plus this writer's record (MemFile::copy).
CopyBuild(w) ==
  \* The writer is about to append, and a buffer number is left.
  /\ wpc[w] = "start"
  \* (A buffer number is left.)
  /\ nextExt <= MaxExt
  \* The new buffer: the published records, then this one.
  /\ wnew' = [wnew EXCEPT ![w] = SubSeq(buf[cur], 1, len[cur]) \o <<w>>]
  \* The writer remembers the buffer and the state word it saw.
  /\ wext' = [wext EXCEPT ![w] = cur]
  \* (Its published length.)
  /\ wlen' = [wlen EXCEPT ![w] = len[cur]]
  \* (The two bits it saw.)
  /\ wsaw' = [wsaw EXCEPT ![w] = [bit |-> wbit[cur], frz |-> frozen[cur]]]
  \* The writer has built its copy.
  /\ wpc' = [wpc EXCEPT ![w] = "built"]
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, cur, buf, len, wbit, frozen, nextExt, finished,
                 rpc, rfin, rext, rlen, rgot>>

\* A copy, step 2: freeze the old buffer with a CAS on the word it saw when
\* it built, so no append publishes there any more. If that word was already
\* frozen, the length it copied is final and there is nothing to do. The
\* choice is made from the word seen at build time, as the code makes it
\* (MemFile::copy tests the `state` it loaded first): a buffer someone else
\* froze AFTER the build may have published more records before that freeze.
\* The bug NoFreeze skips the freeze.
CopyFreeze(w) ==
  \* The writer built its new buffer.
  /\ wpc[w] = "built"
  \* e is the buffer the writer copied from.
  /\ LET e == wext[w] IN
       \* The bug NoFreeze, or a buffer frozen when it built: go straight to
       \* the swap. The bug StaleFrozen also goes straight on when the buffer
       \* is frozen NOW, though its length may have grown since the build.
       IF \/ Mutant = "NoFreeze"
          \* or it saw the buffer frozen already;
          \/ wsaw[w].frz
          \* or, under the bug StaleFrozen, the buffer is frozen now.
          \/ Mutant = "StaleFrozen" /\ frozen[e]
       \* The writer goes on to the swap.
       THEN /\ wpc' = [wpc EXCEPT ![w] = "frozen"]
            \* No bit changes.
            /\ UNCHANGED frozen
       \* The word is still what it saw: the CAS sets FROZEN.
       ELSE IF len[e] = wlen[w] /\ wbit[e] = wsaw[w].bit /\ ~frozen[e]
            \* The buffer is frozen.
            THEN /\ frozen' = [frozen EXCEPT ![e] = TRUE]
                 \* The writer goes on to the swap.
                 /\ wpc' = [wpc EXCEPT ![w] = "frozen"]
            \* The word changed (an append published, or another copy froze
            \* it): start over.
            ELSE /\ wpc' = [wpc EXCEPT ![w] = "start"]
                 \* No bit changes.
                 /\ UNCHANGED frozen
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, cur, buf, len, wbit, nextExt, finished, wext, wlen,
                 wsaw, wnew, rpc, rfin, rext, rlen, rgot>>

\* A copy, step 3: swap the new buffer in with a CAS on the file's pointer.
\* It fails if another copy swapped first; then the writer starts over.
CopySwap(w) ==
  \* The old buffer is frozen (or the bug skipped it).
  /\ wpc[w] = "frozen"
  \* The pointer still names the old buffer: the CAS wins ...
  /\ IF cur = wext[w]
     \* The pointer names the new buffer ...
     THEN /\ cur' = nextExt
          \* ... and the new buffer holds what the writer built, all published.
          /\ buf' = [buf EXCEPT ![nextExt] = wnew[w]]
          \* (Its published length is all of it.)
          /\ len' = [len EXCEPT ![nextExt] = Len(wnew[w])]
          \* The next new buffer takes the next number.
          /\ nextExt' = nextExt + 1
          \* The append has returned,
          /\ finished' = finished \cup {w}
          \* and the writer is done.
          /\ wpc' = [wpc EXCEPT ![w] = "done"]
     \* ... otherwise another copy won: start over.
     ELSE /\ wpc' = [wpc EXCEPT ![w] = "start"]
          \* Nothing is swapped.
          /\ UNCHANGED <<cur, buf, len, nextExt, finished>>
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, wbit, frozen, wext, wlen, wsaw, wnew, rpc, rfin, rext, rlen, rgot>>

\* A memory read, step 1: it begins; note the appends finished by now.
ReadBegin(r) ==
  \* The reader has not begun.
  /\ rpc[r] = "start"
  \* Every append that returned before this moment must be seen.
  /\ rfin' = [rfin EXCEPT ![r] = finished]
  \* The read has begun.
  /\ rpc' = [rpc EXCEPT ![r] = "begun"]
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, cur, buf, len, wbit, frozen, nextExt, finished, wpc, wext,
                 wlen, wsaw, wnew, rext, rlen, rgot>>

\* A memory read, step 2: load the file's current buffer.
ReadLoad(r) ==
  \* The reader began.
  /\ rpc[r] = "begun"
  \* It holds the buffer the pointer names now.
  /\ rext' = [rext EXCEPT ![r] = cur]
  \* The reader has loaded the pointer.
  /\ rpc' = [rpc EXCEPT ![r] = "loaded"]
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, cur, buf, len, wbit, frozen, nextExt, finished, wpc, wext,
                 wlen, wsaw, wnew, rfin, rlen, rgot>>

\* A memory read, step 3: load that buffer's published length.
ReadLen(r) ==
  \* The reader holds a buffer.
  /\ rpc[r] = "loaded"
  \* It reads the length published on it now.
  /\ rlen' = [rlen EXCEPT ![r] = len[rext[r]]]
  \* The reader has read the length.
  /\ rpc' = [rpc EXCEPT ![r] = "lenread"]
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, cur, buf, len, wbit, frozen, nextExt, finished, wpc, wext,
                 wlen, wsaw, wnew, rfin, rext, rgot>>

\* A memory read, step 4: copy the records below that length.
ReadCopy(r) ==
  \* The reader has its length.
  /\ rpc[r] = "lenread"
  \* It copies the published records of its buffer.
  /\ rgot' = [rgot EXCEPT ![r] = SubSeq(buf[rext[r]], 1, rlen[r])]
  \* The read is done.
  /\ rpc' = [rpc EXCEPT ![r] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<diskVars, cur, buf, len, wbit, frozen, nextExt, finished, wpc, wext,
                 wlen, wsaw, wnew, rfin, rext, rlen>>

\* ---------------------------------------------------------------------------
\* Part 3: a table renamed aside while its handles reopen it.
\* ---------------------------------------------------------------------------

\* The names the table's entry records (PathEntry::names), by how many
\* times the move has stored them: 0 before the move, 1 while the rename
\* runs (both names, the old as "previous"), 2 once it settled.
NamesAt(v) ==
  \* Before the move: only "A".
  IF v = 0 THEN [cur |-> "A", prev |-> "none"]
  \* During the rename: "B" now, and "A" as where it was.
  ELSE IF v = 1 THEN [cur |-> "B", prev |-> "A"]
  \* Settled: only "B".
  ELSE [cur |-> "B", prev |-> "none"]

\* Which names the move has stored so far, at phase p: 0 at phase 0, 1
\* through phases 1 to 3 (the rename runs in between), 2 at phase 4.
Version(p) ==
  \* Nothing stored yet.
  IF p = 0 THEN 0
  \* The settled names are the last store.
  ELSE IF p = 4 THEN 2
  \* The names with "previous" stand while the Env renames.
  ELSE 1

\* Whether name n holds the file now. The Env's rename is two map writes,
\* phases 2 and 3: the real one adds "B" (phase 2) before it takes "A"
\* away (phase 3); the bug RemoveFirst takes "A" away first.
Holds(n) ==
  \* The old name, "A".
  IF n = "A"
  \* It holds the file until its removal.
  THEN IF Mutant = "RemoveFirst" THEN mph < 2 ELSE mph < 3
  \* The new name, "B": it holds the file from its insertion on.
  ELSE IF Mutant = "RemoveFirst" THEN mph >= 3 ELSE mph >= 2

\* The mover takes its next step, each one atomic: store both names (1),
\* the Env's first map write (2), its second (3), store the settled names
\* (4). Only when there are reopens to race, so the other configurations
\* keep this part still.
Move ==
  \* There is a reopen to race ...
  /\ Openers # {}
  \* ... and the move is not done.
  /\ mph < 4
  \* One step further.
  /\ mph' = mph + 1
  \* No reopen moves in this step.
  /\ UNCHANGED <<opc, over>>

\* A reopen loads the names the entry records now (the Atom load).
OLoad(o) ==
  \* It is about to load.
  /\ opc[o] = "load"
  \* It holds the names stored so far.
  /\ over' = [over EXCEPT ![o] = Version(mph)]
  \* It tries "previous" first when there is one, else "current".
  /\ opc' = [opc EXCEPT ![o] = IF NamesAt(Version(mph)).prev # "none" THEN "prev" ELSE "cur"]
  \* The move does not step here.
  /\ UNCHANGED mph

\* A reopen opens its "previous" name: found, or it tries "current".
OPrev(o) ==
  \* It is trying "previous".
  /\ opc[o] = "prev"
  \* The file is there, or it goes on to "current".
  /\ opc' = [opc EXCEPT ![o] = IF Holds(NamesAt(over[o]).prev) THEN "found" ELSE "cur"]
  \* Nothing else changes.
  /\ UNCHANGED <<mph, over>>

\* A reopen opens its "current" name: found, or it looks at the names again.
OCur(o) ==
  \* It is trying "current".
  /\ opc[o] = "cur"
  \* The file is there, or it goes to look at the names again.
  /\ opc' = [opc EXCEPT ![o] = IF Holds(NamesAt(over[o]).cur) THEN "found" ELSE "reload"]
  \* Nothing else changes.
  /\ UNCHANGED <<mph, over>>

\* A reopen whose names held nothing loads them again: if a move stored new
\* ones since, it tries those; if not, the file is really gone and it
\* reports NotFound. The bug NoReload always reports NotFound here.
OReload(o) ==
  \* Both its names held nothing.
  /\ opc[o] = "reload"
  \* Newer names: start over with them; the same ones: give up.
  /\ opc' = [opc EXCEPT ![o] =
               \* (newer names, and not the bug: load them; else: NotFound)
               IF Version(mph) # over[o] /\ Mutant # "NoReload" THEN "load" ELSE "lost"]
  \* Nothing else changes.
  /\ UNCHANGED <<mph, over>>

\* Every step anyone can take.
Next ==
  \* Parts 1 and 2 (Part 3's names do not change): a disk reader reads at its own offset, or
  \* seeks, or reads at the shared cursor;
  \/ /\ \/ \E r \in DiskReaders : PRead(r) \/ Seek(r) \/ ReadAtCursor(r)
        \* or a writer
        \/ \E w \in Writers :
             \* appends in place (claim, write, publish)
             \/ InPlaceClaim(w) \/ InPlaceWrite(w) \/ InPlacePublish(w)
             \* or by copying (build, freeze, swap);
             \/ CopyBuild(w) \/ CopyFreeze(w) \/ CopySwap(w)
        \* or a memory reader takes one of its four steps.
        \/ \E r \in MemReaders : ReadBegin(r) \/ ReadLoad(r) \/ ReadLen(r) \/ ReadCopy(r)
     \* (Part 3 stays still.)
     /\ UNCHANGED nameVars
  \* Part 3: the move, or a reopen's step; Parts 1 and 2 stay still.
  \/ /\ \/ Move
        \* Or a reopen's step.
        \/ \E o \in Openers : OLoad(o) \/ OPrev(o) \/ OCur(o) \/ OReload(o)
     \* (Parts 1 and 2 stay still.)
     /\ UNCHANGED <<diskVars, memVars>>

\* The behaviours: start in Init, take Next steps.
Spec == Init /\ [][Next]_vars

\* ===========================================================================
\* THE RULES
\* ===========================================================================

\* A disk read returns the bytes at its own offset. Rules out: A seeks to 0,
\* B seeks to 1, A reads at the shared cursor and gets offset 1's bytes.
ReadsOwnBytes ==
  \* Every disk reader that read got the bytes of its own offset.
  \A r \in DiskReaders : ppc[r] = "done" => pgot[r] = Data(At[r])

\* The current buffer's published records hold every finished append.
\* Rules out: an append publishing on a buffer a copy already replaced, so
\* it "finished" where nobody will ever read.
CurrentHoldsFinished ==
  \* Every finished append is among the current buffer's published records.
  finished \subseteq Range(SubSeq(buf[cur], 1, len[cur]))

\* A memory read sees every append that finished before it began. Rules
\* out: a reader that began after an append returned and missed it.
ReadSeesFinished ==
  \* Every memory reader that is done got every append finished at its start.
  \A r \in MemReaders : rpc[r] = "done" => rfin[r] \subseteq Range(rgot[r])

\* No record is published twice in the current buffer. Rules out: an
\* append retried after a lost race that also landed the first time.
AtMostOnce ==
  \* The current buffer's published records ...
  LET s == SubSeq(buf[cur], 1, len[cur]) IN
    \* ... hold no record at two positions.
    \A i, j \in 1..Len(s) : s[i] = s[j] => i = j

\* A reopen always finds the file, however the move and it interleave.
\* Rules out: the rename takes "A" away, a reopen tries "A" and then "B"
\* before "B" is added, and the read fails on a table that exists. Rules
\* out too: a reopen that knew only "A", while the whole move ran, giving
\* up on "A" instead of looking at the names again.
OpenFindsFile ==
  \* No reopen ever ends up with nothing.
  \A o \in Openers : opc[o] # "lost"

====
