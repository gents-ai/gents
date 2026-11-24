---- MODULE MemFileExtents ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A file held in memory (MemFile, src/env/mem_file.rs, behind MemEnv and
\* the OPFS mirror) is read with no lock. Its bytes live in an "extent": a
\* chain of buffers ("segments", src/env/mem_file/extent.rs) and one word
\* that holds the published length L and two flags, WRITER and FROZEN. A
\* reader loads the file's current extent (a kovan Atom, which also keeps
\* the extent alive while the reader holds it), loads L, and copies the
\* bytes below L, walking the chain.
\*
\* How a write changes the file:
\*   - An append at the end, "in place": take WRITER with a CAS, link one
\*     more segment at the end of the chain if the room runs out (as large
\*     as everything before it), copy the bytes above L, and publish the
\*     new length with a second CAS that clears WRITER. A file of n bytes
\*     is about log2(n) segments, and growing never copies.
\*   - Everything else (an overwrite, a shrink, an append that meets
\*     another one in place) builds a NEW extent of one segment from the
\*     published bytes and its own, FREEZES the old one with a CAS so
\*     nothing publishes there any more, and swaps the new one in with a
\*     CAS on the Atom. The old extent is then retired: kovan frees it once
\*     nobody holds it. One of 64 KiB or more is handed to the reclaimer at
\*     once (engine::reclaim::submit), after the writer let go of it.
\*
\* Room a segment has but nobody wrote is UNINITIALIZED memory. So a write
\* that makes the file longer without bytes of its own (set_len to a larger
\* length, a write past the end) zeroes what it exposes, explicitly.
\*
\* Tiny examples of what goes wrong without each rule:
\*   - set_len(3) on a 1-byte file, in place, without zeroing: the room it
\*     linked holds whatever the allocator left there, and a reader of
\*     bytes 1 and 2 copies garbage.
\*   - publishing the length before linking the segment: a reader loads the
\*     new length, walks the chain, and the segment holding its last byte
\*     is not there yet.
\*   - a reader that lets go of the extent before it copies: the extent is
\*     replaced, nobody holds it, kovan frees it, and the copy reads freed
\*     memory.
\*
\* What this model checks cannot happen:
\*   - the current extent's published bytes differ from the file the writes
\*     made so far, or hold uninitialized or unreachable bytes
\*     (CurrentIsFile);
\*   - any extent still alive has an uninitialized or unreachable byte
\*     below its published length (PublishedInitialized);
\*   - a read returns anything but the whole file as it stood at one moment
\*     during the read: never a byte past the length, never one not yet
\*     written, never half old and half new (ReadIsSnapshot);
\*   - a thread uses an extent after it was freed (NoUseAfterFree), or a
\*     freed extent is still reachable (FreedUnreachable);
\*   - with fairness, a replaced extent of the prompt size is never freed
\*     (LargeFreed, a property of whole runs).
\*
\* WHAT IS MODELLED, and the code each piece mirrors (file and line of the
\* tree the model was checked against; mem_file.rs unless named):
\*   segs, mem    an extent's chain: each segment's room (Segment::bytes,
\*                extent.rs:43) in order, and the bytes of all of them laid
\*                end to end. A byte past the chain is Gone.
\*   elen, wbit, frz
\*                the LEN, WRITER and FROZEN parts of Extent::state
\*                (extent.rs:97; LEN, WRITER, FROZEN at 56-60).
\*   cur          MemFile::current (164), the Atom readers load.
\*   retired, handed, freed
\*                extents a winning Atom CAS replaced (kovan retires them,
\*                kovan atom.rs compare_and_swap); those submit() handed on
\*                (315); those kovan freed.
\*   guardW, guardR
\*                the AtomGuard each writer and reader holds (284, 201,
\*                219, 235): kovan frees no extent a guard holds.
\*   file         the file as the writes made it so far (the spec).
\*   wpc..wrcap   each writer's step and locals; rpc..rseen each reader's.
\*
\*   Model step   Rust
\*   WLoad        change: `self.current.load()` (284)
\*   WState       change: `settled(&extent)`, bounds, the in-place test
\*                (285-297)
\*   Claim        in_place: CAS state -> state | WRITER (343-349)
\*   Reserve      in_place: `extent.reserve(to)` (351), Extent::reserve
\*                (extent.rs:161-180), the link a Release store (178)
\*   Zero         in_place: set_len zeroes what it exposes (362-366)
\*   Fill         in_place: `change.fill(extent, from)` (369)
\*   Publish      in_place: CAS state | WRITER -> end (377-380)
\*   Build        copy: capacity, Extent::with_capacity, copy_prefix, the
\*                gap's zero, fill (397-424)
\*   Freeze       copy: CAS state -> state | FROZEN unless it was frozen
\*                (432-439)
\*   Swap         copy: `self.current.compare_and_swap(extent, fresh)` (440)
\*   Cap          change: `replaced = extent.capacity()` (312)
\*   Release      change: `drop(extent)` after an append in place (313)
\*   LetGo        change: `drop(extent)` after a copy (313)
\*   Drop         change: the end of a loop turn that lost a race (321-323)
\*   Submit       change: `reclaim::submit()` at RECLAIM_PROMPTLY (314-318)
\*   RLoad, RLen, RCopy, RRelease
\*                read_exact_at / to_vec: load, `state.load(Acquire) & LEN`,
\*                copy_out (extent.rs:223-232), the guard dropped (215-241)
\*   Free         kovan frees a retired extent nobody holds; FreeHanded is
\*                the same for one submit() handed on, which kovan does
\*                promptly
\*
\*   Rule                 Rust it holds for
\*   CurrentIsFile        the zeroing (365, 419-421), link before publish
\*                        (extent.rs:178 before 377), in place only at the
\*                        end (297)
\*   PublishedInitialized the same, on every extent alive
\*   ReadIsSnapshot       read_exact_at copies below the length it loaded
\*                        (220-228)
\*   NoUseAfterFree       the guard held across every use (201-210,
\*                        284-319): the capacity read before the drop (312)
\*   LargeFreed           the submit (314-318)
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Byte values: a writer writes its own number, the first file holds
\*     7s, zero is 0; Uninit and Gone stand for uninitialized room and a
\*     byte no linked segment holds.
\*   - Memory ordering: every step here is atomic and in one order. The
\*     Release link, the Release publication and the Acquire loads that make
\*     them so are checked by tests/loom_mem_file.rs.
\*   - The copy's freeze racing an append in place, the stale-frozen test,
\*     and append_at's one-length condition: EnvFiles.tla.
\*   - How kovan decides that nobody holds an extent: its own contract. Here
\*     Free needs no guard on the extent, which is that contract; what this
\*     model checks is that regolith holds a guard across every use.
\*   - The Charge an OPFS mirror reserves for a write, and a length too
\*     large to address: each ends the write before it changes anything.
\*   - A failed allocation in Reserve. in_place has taken WRITER (Claim);
\*     the allocation fails before anything is linked, and in_place gives
\*     WRITER back with a second CAS, state | WRITER to state, and returns
\*     the error (351-360). If a copy froze the extent meanwhile, that CAS
\*     fails and the word stays as the freeze left it: the file was taken
\*     elsewhere. No byte, link or length changes, so the path leaves only
\*     states this model reaches without it: the word as it was before the
\*     Claim, or the frozen word a Freeze makes while a writer holds WRITER
\*     (Freeze matches the WRITER bit it loaded). Its error ends that write
\*     and leaves the file as it was.
\*   - Where in the chain each byte lies: mem lays the segments end to end,
\*     so a byte's place is its offset. That Extent::pieces cuts every
\*     range into pieces that each stay inside their segment and cover the
\*     range once, in order; that Extent::reserve leaves room for the whole
\*     write and at least doubles the room; and that a link moves no byte,
\*     hold for every chain and every range: proofs/lean/Regolith/
\*     MemFileExtents.lean proves them.
\*   - Several writes by one writer: one write each, by two writers, shows
\*     every race between two; the chain grows from nothing to three
\*     segments within that.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one rule
\* it names):
\*   MC_MemFileExtents_Green_Appends    an empty file, two appends of one
\*                                      byte, one reader: the chain grows
\*                                      in place, or one append copies.
\*   MC_MemFileExtents_Green_Extend     an append and a set_len that grows
\*                                      the file, in place or by a copy.
\*   MC_MemFileExtents_Green_Shrink     an append and a set_len that cuts
\*                                      the file (always a copy).
\*   MC_MemFileExtents_Green_Overwrite  an overwrite below the length and a
\*                                      write past the end (a gap).
\*   MC_MemFileExtents_Green_Reclaim    a copy that replaces a large extent
\*                                      while a reader holds it, with
\*                                      fairness: LargeFreed holds.
\*   MC_MemFileExtents_Red_NoZeroExtend set_len in place does not zero:
\*                                      CurrentIsFile breaks.
\*   MC_MemFileExtents_Red_NoZeroGap    a copy does not zero the gap:
\*                                      CurrentIsFile breaks.
\*   MC_MemFileExtents_Red_PublishFirst an append publishes its length
\*                                      before it links and fills:
\*                                      CurrentIsFile breaks.
\*   MC_MemFileExtents_Red_OverwriteInPlace
\*                                      an overwrite writes in place, below
\*                                      the published length: CurrentIsFile
\*                                      breaks.
\*   MC_MemFileExtents_Red_ReadsRoom    a reader copies the room, not the
\*                                      length: ReadIsSnapshot breaks.
\*   MC_MemFileExtents_Red_ReadAfterRelease
\*                                      a reader lets go of the extent
\*                                      before it copies: NoUseAfterFree
\*                                      breaks.
\*   MC_MemFileExtents_Red_CapAfterDrop a writer reads the replaced
\*                                      extent's capacity after it let go:
\*                                      NoUseAfterFree breaks.
\*   MC_MemFileExtents_Red_NoSubmit     a replaced large extent is not
\*                                      handed to the reclaimer: LargeFreed
\*                                      breaks.
\*   MC_MemFileExtents_Green_ReachesGrowthRead
\*                                      Green_Appends with the witness
\*                                      NeverReadsGrowing, which must break:
\*                                      a reader copies from an extent while
\*                                      a writer grows it in place.
\*   MC_MemFileExtents_Green_ReachesRetiredRead
\*                                      Green_Shrink with the witness
\*                                      NeverReadsRetired, which must break:
\*                                      a reader copies from an extent a
\*                                      copy already replaced.

\* Numbers, sequences, and the size of a set.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The writer threads, numbered from 1; writer w writes the byte w.
  Writers,
  \* [Writers -> op]: the one write each makes (see the ops below).
  Ops,
  \* The reader threads; each reads the whole file once.
  Readers,
  \* How many bytes of 7 the file holds at the start.
  InitLen,
  \* The room of the first extent's one segment.
  InitCap,
  \* MIN_CAPACITY: the smallest segment a file grows by.
  MinCap,
  \* RECLAIM_PROMPTLY: from this room on, a replaced extent is submitted.
  Big,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The bug names this model knows.
ASSUME Mutant \in {"none", "NoZeroExtend", "NoZeroGap", "PublishFirst", "OverwriteInPlace",
                   \* and the reader's and the reclaimer's.
                   "ReadsRoom", "ReadAfterRelease", "CapAfterDrop", "NoSubmit"}
\* The first extent holds the first bytes.
ASSUME InitLen <= InitCap

\* A byte of uninitialized room.
Uninit == 99
\* A byte no linked segment holds.
Gone == 98
\* The bytes the first file holds.
First == 7
\* The extents: the first, and one per copy that can win (one per writer).
Exts == 1..(1 + Cardinality(Writers))
\* No extent (a guard that holds nothing).
NoExt == 0

\* The larger of two numbers.
Max(a, b) == IF a > b THEN a ELSE b
\* The smaller of two numbers.
Min(a, b) == IF a < b THEN a ELSE b
\* The larger of three.
Max3(a, b, c) == Max(Max(a, b), c)
\* The sum of a sequence of numbers.
RECURSIVE Sum(_)
\* Empty: 0; else the first plus the rest.
Sum(s) == IF s = <<>> THEN 0 ELSE Head(s) + Sum(Tail(s))
\* n copies of byte v.
Bytes(n, v) == [i \in 1..n |-> v]

\* The ops. An append of n bytes at the end.
Append_(n) == [kind |-> "append", at |-> 0, n |-> n]
\* set_len(n): cut the file to n bytes or extend it with zeros.
SetLen(n) == [kind |-> "setlen", at |-> 0, n |-> n]
\* write_at(a, n bytes): an overwrite, or a write past the end.
WriteAt(a, n) == [kind |-> "at", at |-> a, n |-> n]

\* For the configurations: writers 1 and 2 each append one byte.
OpsAppends == [w \in {1, 2} |-> Append_(1)]
\* Writer 1 appends a byte; writer 2 makes the file 3 bytes long.
OpsExtend == [w \in {1, 2} |-> IF w = 1 THEN Append_(1) ELSE SetLen(3)]
\* Writer 1 appends a byte; writer 2 cuts the file to 1 byte.
OpsShrink == [w \in {1, 2} |-> IF w = 1 THEN Append_(1) ELSE SetLen(1)]
\* Writer 1 overwrites byte 0; writer 2 writes byte 2, past the end.
OpsOverwrite == [w \in {1, 2} |-> IF w = 1 THEN WriteAt(0, 1) ELSE WriteAt(2, 1)]

\* Where op starts and the length after it, for a file now len bytes long
\* (Change::bounds, 105-122).
Start(op, len) ==
  \* An append starts at the end; set_len at the shorter length; a write at
  \* its offset.
  CASE op.kind = "append" -> len
    \* (set_len)
    [] op.kind = "setlen" -> Min(len, op.n)
    \* (write_at)
    [] op.kind = "at" -> op.at
\* The length after op.
End(op, len) ==
  \* An append adds n; set_len makes it n; a write reaches its end or keeps
  \* the length.
  CASE op.kind = "append" -> len + op.n
    \* (set_len)
    [] op.kind = "setlen" -> op.n
    \* (write_at)
    [] op.kind = "at" -> Max(op.at + op.n, len)

\* The file after writer w's op: what readers must see once it lands.
Apply(op, w, f) ==
  \* Bytes at and past the write's start are w's within its n bytes; the
  \* rest are the old bytes, or zero past the old end.
  CASE op.kind = "append" -> f \o Bytes(op.n, w)
    \* (set_len: cut, or zeros added)
    [] op.kind = "setlen" ->
         IF op.n <= Len(f) THEN SubSeq(f, 1, op.n) ELSE f \o Bytes(op.n - Len(f), 0)
    \* (write_at: w's bytes over the old ones, zeros in a gap)
    [] op.kind = "at" ->
         [i \in 1..Max(op.at + op.n, Len(f)) |->
            IF i - 1 >= op.at /\ i - 1 < op.at + op.n THEN w
            ELSE IF i <= Len(f) THEN f[i] ELSE 0]

\* m with bytes [from, to) set to v; bytes past m's end stay unwritten, as
\* `pieces` stops where the chain ends (extent.rs:207-212).
Write(m, from, to, v) ==
  \* Inside [from, to): v; elsewhere unchanged.
  [i \in DOMAIN m |-> IF i - 1 >= from /\ i - 1 < to THEN v ELSE m[i]]

\* The state that changes from step to step.
VARIABLES
  \* [Exts -> Seq(Nat)]: each extent's segments' room, in chain order.
  segs,
  \* [Exts -> Seq(byte)]: the bytes of those segments end to end.
  mem,
  \* [Exts -> Nat]: the published length.
  elen,
  \* [Exts -> BOOLEAN]: the WRITER bit.
  wbit,
  \* [Exts -> BOOLEAN]: the FROZEN bit.
  frz,
  \* The extent the Atom holds.
  cur,
  \* The next extent number a winning copy gets.
  nextExt,
  \* The extents a copy replaced.
  retired,
  \* The replaced extents submit() handed to the reclaimer.
  handed,
  \* The extents kovan freed.
  freed,
  \* The file as the writes that landed made it.
  file,
  \* Ghost: the extents a writer found large after replacing them.
  bigRetired,
  \* [Writers -> step]: where each writer is.
  wpc,
  \* [Writers -> Exts \cup {NoExt}]: the extent its guard holds.
  guardW,
  \* [Writers -> Exts]: the extent it works on.
  wext,
  \* [Writers -> [len, bit, frz]]: the state word it loaded.
  wst,
  \* [Writers -> [segs, mem, len]]: the extent a copy built.
  wfresh,
  \* [Writers -> Nat]: the replaced extent's room it read.
  wrcap,
  \* [Readers -> step]: where each reader is.
  rpc,
  \* [Readers -> Exts \cup {NoExt}]: the extent its guard holds.
  guardR,
  \* [Readers -> Exts]: the extent it loaded.
  rext,
  \* [Readers -> Nat]: the length it loaded.
  rlen,
  \* [Readers -> Seq(byte)]: what it copied.
  rgot,
  \* [Readers -> SUBSET Seq(byte)]: every file there was during its read.
  rseen

\* The extents' variables.
extVars == <<segs, mem, elen, wbit, frz>>
\* The file pointer, the reclaimer's sets and the spec.
fileVars == <<cur, nextExt, retired, handed, freed, file, bigRetired>>
\* The writers' variables.
writerVars == <<wpc, guardW, wext, wst, wfresh, wrcap>>
\* The readers' variables.
readerVars == <<rpc, guardR, rext, rlen, rgot, rseen>>
\* Every variable.
vars == <<extVars, fileVars, writerVars, readerVars>>

\* An extent's room: every linked segment's (Extent::capacity, extent.rs:145).
Room(e) == Sum(segs[e])
\* The byte at 0-based offset i of extent e, walking its chain.
ByteAt(e, i) == IF i < Len(mem[e]) THEN mem[e][i + 1] ELSE Gone
\* The published bytes of extent e.
Content(e) == [i \in 1..elen[e] |-> ByteAt(e, i - 1)]
\* Every extent some thread holds a guard on.
Guarded == {guardW[w] : w \in Writers} \cup {guardR[r] : r \in Readers}

\* The steps a writer takes, by name.
WSteps == {"load", "state", "claim", "reserve", "zero", "fill", "publish", "build", "freeze",
           "swap", "cap", "release", "letgo", "submit", "drop", "done"}

\* Every variable holds the kind of value it should.
TypeOK ==
  \* Segment rooms are numbers.
  /\ segs \in [Exts -> Seq(Nat)]
  \* Lengths are numbers.
  /\ elen \in [Exts -> Nat]
  \* The bits are booleans.
  /\ wbit \in [Exts -> BOOLEAN] /\ frz \in [Exts -> BOOLEAN]
  \* The Atom holds an extent.
  /\ cur \in Exts
  \* The reclaimer's sets are sets of extents.
  /\ retired \subseteq Exts /\ handed \subseteq retired /\ freed \subseteq retired
  \* Each writer is at one of its steps.
  /\ wpc \in [Writers -> WSteps]
  \* Each reader is at one of its steps.
  /\ rpc \in [Readers -> {"start", "load", "len", "unguard", "copy", "end", "done"}]

\* The start: the first extent holds InitLen bytes of 7 in one segment of
\* InitCap room; nobody has done anything.
Init ==
  \* Extent 1 has one segment; the others are not made yet.
  /\ segs = [e \in Exts |-> IF e = 1 THEN <<InitCap>> ELSE <<>>]
  \* Its bytes: the file, then uninitialized room.
  /\ mem = [e \in Exts |-> IF e = 1 THEN Bytes(InitLen, First) \o Bytes(InitCap - InitLen, Uninit)
                                    ELSE <<>>]
  \* Its length is the file's.
  /\ elen = [e \in Exts |-> IF e = 1 THEN InitLen ELSE 0]
  \* No WRITER.
  /\ wbit = [e \in Exts |-> FALSE]
  \* Nothing frozen.
  /\ frz = [e \in Exts |-> FALSE]
  \* Readers load extent 1.
  /\ cur = 1
  \* A winning copy makes extent 2.
  /\ nextExt = 2
  \* Nothing replaced,
  /\ retired = {}
  \* handed on,
  /\ handed = {}
  \* or freed.
  /\ freed = {}
  \* The file is the first bytes.
  /\ file = Bytes(InitLen, First)
  \* Nothing large replaced.
  /\ bigRetired = {}
  \* Every writer is about to load the extent.
  /\ wpc = [w \in Writers |-> "load"]
  \* Holding no guard.
  /\ guardW = [w \in Writers |-> NoExt]
  \* Its extent is a placeholder.
  /\ wext = [w \in Writers |-> 1]
  \* As is the word it loaded.
  /\ wst = [w \in Writers |-> [len |-> 0, bit |-> FALSE, frz |-> FALSE]]
  \* It built nothing.
  /\ wfresh = [w \in Writers |-> [segs |-> <<>>, mem |-> <<>>, len |-> 0]]
  \* Nor read a room.
  /\ wrcap = [w \in Writers |-> 0]
  \* Every reader is about to start.
  /\ rpc = [r \in Readers |-> "start"]
  \* Holding no guard.
  /\ guardR = [r \in Readers |-> NoExt]
  \* Its extent is a placeholder.
  /\ rext = [r \in Readers |-> 1]
  \* No length loaded.
  /\ rlen = [r \in Readers |-> 0]
  \* Nothing copied.
  /\ rgot = [r \in Readers |-> <<>>]
  \* No file seen.
  /\ rseen = [r \in Readers |-> {}]

\* A write lands: the file becomes nf, and every reader in the middle of a
\* read notes that the file was nf for a moment.
Land(nf) ==
  \* The file.
  /\ file' = nf
  \* Each reader between its start and its end.
  /\ rseen' = [r \in Readers |->
                IF rpc[r] \notin {"start", "done"} THEN rseen[r] \cup {nf} ELSE rseen[r]]

\* ---------------------------------------------------------------------------
\* Writers (MemFile::change).
\* ---------------------------------------------------------------------------

\* Writer w's op.
OpOf(w) == Ops[w]
\* Where it starts and ends, for the length it loaded.
WFrom(w) == Start(OpOf(w), wst[w].len)
\* (The length after it.)
WTo(w) == End(OpOf(w), wst[w].len)

\* change: load the current extent; the guard holds it from here.
WLoad(w) ==
  \* About to load.
  /\ wpc[w] = "load"
  \* The extent the Atom holds now.
  /\ wext' = [wext EXCEPT ![w] = cur]
  \* Held by the guard.
  /\ guardW' = [guardW EXCEPT ![w] = cur]
  \* Then its state word.
  /\ wpc' = [wpc EXCEPT ![w] = "state"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, wst, wfresh, wrcap, readerVars>>

\* change: load the state word; an append at the end of an extent nobody
\* writes or froze goes in place, anything else copies (297). The bug
\* OverwriteInPlace drops the "at the end" test.
WState(w) ==
  \* About to load the word.
  /\ wpc[w] = "state"
  \* What the word says now.
  /\ wst' = [wst EXCEPT ![w] = [len |-> elen[wext[w]], bit |-> wbit[wext[w]],
                                frz |-> frz[wext[w]]]]
  \* In place or by a copy.
  /\ LET len == elen[wext[w]]
         \* (the op's start and end for this length)
         from == Start(OpOf(w), len)
         \* (its end)
         to == End(OpOf(w), len)
         \* (in place: free word, at the end, not shorter)
         inPlace == /\ ~wbit[wext[w]] /\ ~frz[wext[w]] /\ to >= len
                    \* (at the end, unless the bug writes anywhere)
                    /\ (from = len \/ Mutant = "OverwriteInPlace")
     \* The path.
     IN wpc' = [wpc EXCEPT ![w] = IF inPlace THEN "claim" ELSE "build"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, guardW, wext, wfresh, wrcap, readerVars>>

\* in_place: take WRITER with a CAS on the word loaded; a word changed
\* since means another write got there, and the writer goes round again.
Claim(w) ==
  \* About to CAS.
  /\ wpc[w] = "claim"
  \* e is its extent.
  /\ LET e == wext[w] IN
       \* The word is the one loaded.
       IF elen[e] = wst[w].len /\ wbit[e] = wst[w].bit /\ frz[e] = wst[w].frz
       \* WRITER is this writer's.
       THEN /\ wbit' = [wbit EXCEPT ![e] = TRUE]
            \* Then link room; the bug PublishFirst publishes first.
            /\ wpc' = [wpc EXCEPT ![w] = IF Mutant = "PublishFirst" THEN "publish" ELSE "reserve"]
       \* Changed: go round again.
       ELSE /\ wpc' = [wpc EXCEPT ![w] = "drop"]
            \* No bit changes.
            /\ UNCHANGED wbit
  \* Nothing else changes.
  /\ UNCHANGED <<segs, mem, elen, frz, fileVars, guardW, wext, wst, wfresh, wrcap, readerVars>>

\* Extent::reserve: when the room ends before the write does, link one
\* segment at the end of the chain, as large as the room before it, the
\* rest of the write, or MinCap, whichever is largest. Its bytes are
\* uninitialized.
Reserve(w) ==
  \* About to reserve.
  /\ wpc[w] = "reserve"
  \* e is its extent; room its room; to the write's end.
  /\ LET e == wext[w]
         \* (the room now)
         room == Room(e)
         \* (the size of the new segment)
         grow == Max3(WTo(w) - room, room, MinCap)
     \* Room short: link a segment.
     IN IF WTo(w) > room
        \* One more segment.
        THEN /\ segs' = [segs EXCEPT ![e] = Append(@, grow)]
             \* Its bytes, uninitialized.
             /\ mem' = [mem EXCEPT ![e] = @ \o Bytes(grow, Uninit)]
        \* Room enough: nothing.
        ELSE UNCHANGED <<segs, mem>>
  \* Then the zeroing; the bug PublishFirst published already, and goes on
  \* the same way.
  /\ wpc' = [wpc EXCEPT ![w] = "zero"]
  \* Nothing else changes.
  /\ UNCHANGED <<elen, wbit, frz, fileVars, guardW, wext, wst, wfresh, wrcap, readerVars>>

\* in_place: set_len zeroes what it exposes (the room may hold anything);
\* the bug NoZeroExtend does not.
Zero(w) ==
  \* About to zero.
  /\ wpc[w] = "zero"
  \* set_len, and not the bug: the bytes from the old length to the new.
  /\ IF OpOf(w).kind = "setlen" /\ Mutant # "NoZeroExtend"
     \* Zeroed.
     THEN mem' = [mem EXCEPT ![wext[w]] = Write(@, WFrom(w), WTo(w), 0)]
     \* Nothing to zero.
     ELSE UNCHANGED mem
  \* Then the bytes.
  /\ wpc' = [wpc EXCEPT ![w] = "fill"]
  \* Nothing else changes.
  /\ UNCHANGED <<segs, elen, wbit, frz, fileVars, guardW, wext, wst, wfresh, wrcap, readerVars>>

\* in_place: copy the write's bytes in, above the published length.
Fill(w) ==
  \* About to fill.
  /\ wpc[w] = "fill"
  \* An append or a write: its bytes; set_len has none.
  /\ IF OpOf(w).kind = "setlen"
     \* Nothing.
     THEN UNCHANGED mem
     \* Its n bytes from its start are w.
     ELSE mem' = [mem EXCEPT ![wext[w]] = Write(@, WFrom(w), WFrom(w) + OpOf(w).n, w)]
  \* Then publish; the bug PublishFirst published already: let go.
  /\ wpc' = [wpc EXCEPT ![w] = IF Mutant = "PublishFirst" THEN "release" ELSE "publish"]
  \* Nothing else changes.
  /\ UNCHANGED <<segs, elen, wbit, frz, fileVars, guardW, wext, wst, wfresh, wrcap, readerVars>>

\* in_place: publish the new length with a CAS that clears WRITER. It fails
\* only when a copy froze the extent meanwhile; then the writer goes round.
Publish(w) ==
  \* About to publish.
  /\ wpc[w] = "publish"
  \* e is its extent.
  /\ LET e == wext[w] IN
       \* Still WRITER, not frozen: the length lands.
       IF wbit[e] /\ ~frz[e] /\ elen[e] = wst[w].len
       \* The new length.
       THEN /\ elen' = [elen EXCEPT ![e] = WTo(w)]
            \* WRITER cleared.
            /\ wbit' = [wbit EXCEPT ![e] = FALSE]
            \* The write landed.
            /\ Land(Apply(OpOf(w), w, file))
            \* Let go; the bug PublishFirst still has to link and fill.
            /\ wpc' = [wpc EXCEPT ![w] = IF Mutant = "PublishFirst" THEN "reserve" ELSE "release"]
       \* Frozen: lost; go round.
       ELSE /\ wpc' = [wpc EXCEPT ![w] = "drop"]
            \* Nothing lands.
            /\ UNCHANGED <<elen, wbit, file, rseen>>
  \* Nothing else changes.
  /\ UNCHANGED <<segs, mem, frz, cur, nextExt, retired, handed, freed, bigRetired, guardW, wext,
                 wst, wfresh, wrcap, rpc, guardR, rext, rlen, rgot>>

\* copy: build the new extent: one segment of the room it had (doubled when
\* the write needs more), the published bytes below the length loaded, the
\* gap zeroed (the bug NoZeroGap does not), then the write's bytes.
Build(w) ==
  \* About to build.
  /\ wpc[w] = "build"
  \* The pieces of the new extent.
  /\ LET e == wext[w]
         \* (the old room)
         room == Room(e)
         \* (the write's start and end)
         at == WFrom(w)
         \* (its end)
         to == WTo(w)
         \* (the new room)
         cap == IF to <= room THEN room ELSE Max3(to, 2 * room, MinCap)
         \* (the bytes kept)
         kept == Min(wst[w].len, to)
         \* (the gap: an extension's whole range, or below a write's start)
         gapEnd == IF OpOf(w).kind = "setlen" THEN to ELSE at
         \* (the published prefix, then uninitialized room)
         m1 == [i \in 1..cap |-> IF i <= kept THEN ByteAt(e, i - 1) ELSE Uninit]
         \* (the gap zeroed, unless the bug)
         m2 == IF gapEnd > kept /\ Mutant # "NoZeroGap" THEN Write(m1, kept, gapEnd, 0) ELSE m1
         \* (the write's bytes)
         m3 == IF OpOf(w).kind = "setlen" THEN m2 ELSE Write(m2, at, at + OpOf(w).n, w)
     \* The built extent, its length the write's end.
     IN wfresh' = [wfresh EXCEPT ![w] = [segs |-> <<cap>>, mem |-> m3, len |-> to]]
  \* Then the freeze.
  /\ wpc' = [wpc EXCEPT ![w] = "freeze"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, guardW, wext, wst, wrcap, readerVars>>

\* copy: freeze the old extent with a CAS on the word loaded, unless it was
\* frozen then; a changed word sends the writer round again.
Freeze(w) ==
  \* About to freeze.
  /\ wpc[w] = "freeze"
  \* e is its extent.
  /\ LET e == wext[w] IN
       \* Frozen when loaded: straight to the swap.
       IF wst[w].frz
       \* The swap.
       THEN /\ wpc' = [wpc EXCEPT ![w] = "swap"]
            \* No bit changes.
            /\ UNCHANGED frz
       \* The word is the one loaded: frozen now.
       ELSE IF elen[e] = wst[w].len /\ wbit[e] = wst[w].bit /\ ~frz[e]
       \* FROZEN set.
       THEN /\ frz' = [frz EXCEPT ![e] = TRUE]
            \* Then the swap.
            /\ wpc' = [wpc EXCEPT ![w] = "swap"]
       \* Changed: go round.
       ELSE /\ wpc' = [wpc EXCEPT ![w] = "drop"]
            \* No bit changes.
            /\ UNCHANGED frz
  \* Nothing else changes.
  /\ UNCHANGED <<segs, mem, elen, wbit, fileVars, guardW, wext, wst, wfresh, wrcap, readerVars>>

\* copy: swap the new extent in with a CAS on the Atom. The old one is
\* retired. A lost CAS drops the built extent and goes round.
Swap(w) ==
  \* About to swap.
  /\ wpc[w] = "swap"
  \* The Atom still holds the old extent.
  /\ IF cur = wext[w]
     \* The new extent is published: number nextExt.
     THEN /\ cur' = nextExt
          \* Its segment.
          /\ segs' = [segs EXCEPT ![nextExt] = wfresh[w].segs]
          \* Its bytes.
          /\ mem' = [mem EXCEPT ![nextExt] = wfresh[w].mem]
          \* Its length (`fresh.state.store(end)`, 424).
          /\ elen' = [elen EXCEPT ![nextExt] = wfresh[w].len]
          \* The next number.
          /\ nextExt' = nextExt + 1
          \* The old one is retired.
          /\ retired' = retired \cup {wext[w]}
          \* The write landed.
          /\ Land(Apply(OpOf(w), w, file))
          \* Then read the old room; the bug CapAfterDrop lets go first.
          /\ wpc' = [wpc EXCEPT ![w] = IF Mutant = "CapAfterDrop" THEN "letgo" ELSE "cap"]
     \* Another copy won: go round.
     ELSE /\ wpc' = [wpc EXCEPT ![w] = "drop"]
          \* Nothing is published.
          /\ UNCHANGED <<cur, segs, mem, elen, nextExt, retired, file, rseen>>
  \* Nothing else changes.
  /\ UNCHANGED <<wbit, frz, handed, freed, bigRetired, guardW, wext, wst, wfresh, wrcap, rpc,
                 guardR, rext, rlen, rgot>>

\* change: read the replaced extent's room (`extent.capacity()`).
Cap(w) ==
  \* About to read it.
  /\ wpc[w] = "cap"
  \* Its room now.
  /\ wrcap' = [wrcap EXCEPT ![w] = Room(wext[w])]
  \* Large: noted (a ghost for LargeFreed).
  /\ bigRetired' = IF Room(wext[w]) >= Big THEN bigRetired \cup {wext[w]} ELSE bigRetired
  \* Then let go; the bug CapAfterDrop let go already, and submits.
  /\ wpc' = [wpc EXCEPT ![w] = IF Mutant = "CapAfterDrop" THEN "submit" ELSE "letgo"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, cur, nextExt, retired, handed, freed, file, guardW, wext, wst, wfresh,
                 readerVars>>

\* in_place: the write landed; let go of the extent (`drop(extent)`). Done.
Release(w) ==
  \* About to let go.
  /\ wpc[w] = "release"
  \* The guard holds nothing.
  /\ guardW' = [guardW EXCEPT ![w] = NoExt]
  \* Done.
  /\ wpc' = [wpc EXCEPT ![w] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, wext, wst, wfresh, wrcap, readerVars>>

\* copy: the write landed; let go of the replaced extent (`drop(extent)`),
\* then submit (the bug CapAfterDrop reads the room after this).
LetGo(w) ==
  \* About to let go.
  /\ wpc[w] = "letgo"
  \* The guard holds nothing.
  /\ guardW' = [guardW EXCEPT ![w] = NoExt]
  \* Then submit; with the bug, read the room first.
  /\ wpc' = [wpc EXCEPT ![w] = IF Mutant = "CapAfterDrop" THEN "cap" ELSE "submit"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, wext, wst, wfresh, wrcap, readerVars>>

\* change: a replaced extent of RECLAIM_PROMPTLY or more is handed to the
\* reclaimer now (the bug NoSubmit forgets it). The write is done.
Submit(w) ==
  \* About to submit.
  /\ wpc[w] = "submit"
  \* Large and not the bug: handed on.
  /\ handed' = IF wrcap[w] >= Big /\ Mutant # "NoSubmit" THEN handed \cup {wext[w]} ELSE handed
  \* Done.
  /\ wpc' = [wpc EXCEPT ![w] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, cur, nextExt, retired, freed, file, bigRetired, guardW, wext, wst,
                 wfresh, wrcap, readerVars>>

\* change: a turn of the loop that lost a race ends; its guard goes, and
\* the writer loads the extent again.
Drop(w) ==
  \* At the end of a lost turn.
  /\ wpc[w] = "drop"
  \* The guard holds nothing.
  /\ guardW' = [guardW EXCEPT ![w] = NoExt]
  \* Nothing built is kept.
  /\ wfresh' = [wfresh EXCEPT ![w] = [segs |-> <<>>, mem |-> <<>>, len |-> 0]]
  \* Load again.
  /\ wpc' = [wpc EXCEPT ![w] = "load"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, wext, wst, wrcap, readerVars>>

\* ---------------------------------------------------------------------------
\* Readers (read_exact_at of the whole file, to_vec).
\* ---------------------------------------------------------------------------

\* A read begins: the file as it is now is one it may return.
RStart(r) ==
  \* About to start.
  /\ rpc[r] = "start"
  \* The file now.
  /\ rseen' = [rseen EXCEPT ![r] = {file}]
  \* Then load the extent.
  /\ rpc' = [rpc EXCEPT ![r] = "load"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, writerVars, guardR, rext, rlen, rgot>>

\* Load the current extent; the guard holds it from here.
RLoad(r) ==
  \* About to load.
  /\ rpc[r] = "load"
  \* The extent the Atom holds now.
  /\ rext' = [rext EXCEPT ![r] = cur]
  \* Held by the guard.
  /\ guardR' = [guardR EXCEPT ![r] = cur]
  \* Then its length.
  /\ rpc' = [rpc EXCEPT ![r] = "len"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, writerVars, rlen, rgot, rseen>>

\* Load the extent's published length. The bug ReadAfterRelease lets go
\* of the extent next, before it copies.
RLen(r) ==
  \* About to load the length.
  /\ rpc[r] = "len"
  \* The length published now.
  /\ rlen' = [rlen EXCEPT ![r] = elen[rext[r]]]
  \* Then copy; or, with the bug, let go first.
  /\ rpc' = [rpc EXCEPT ![r] = IF Mutant = "ReadAfterRelease" THEN "unguard" ELSE "copy"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, writerVars, guardR, rext, rgot, rseen>>

\* The bug ReadAfterRelease: the guard goes before the copy.
RUnguard(r) ==
  \* About to let go early.
  /\ rpc[r] = "unguard"
  \* The guard holds nothing.
  /\ guardR' = [guardR EXCEPT ![r] = NoExt]
  \* Then the copy.
  /\ rpc' = [rpc EXCEPT ![r] = "copy"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, writerVars, rext, rlen, rgot, rseen>>

\* copy_out: the bytes below the length loaded, walking the chain. The bug
\* ReadsRoom copies the whole room.
RCopy(r) ==
  \* About to copy.
  /\ rpc[r] = "copy"
  \* How much: the length, or with the bug the room.
  /\ LET n == IF Mutant = "ReadsRoom" THEN Room(rext[r]) ELSE rlen[r]
     \* The bytes.
     IN rgot' = [rgot EXCEPT ![r] = [i \in 1..n |-> ByteAt(rext[r], i - 1)]]
  \* Then let go.
  /\ rpc' = [rpc EXCEPT ![r] = "end"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, writerVars, guardR, rext, rlen, rseen>>

\* The read returns; its guard goes.
REnd(r) ==
  \* About to return.
  /\ rpc[r] = "end"
  \* The guard holds nothing.
  /\ guardR' = [guardR EXCEPT ![r] = NoExt]
  \* Done.
  /\ rpc' = [rpc EXCEPT ![r] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, fileVars, writerVars, rext, rlen, rgot, rseen>>

\* ---------------------------------------------------------------------------
\* The reclaimer (kovan).
\* ---------------------------------------------------------------------------

\* kovan frees a retired extent no guard holds: at some later time for one
\* in a batch still filling.
Free(e) ==
  \* Retired, not freed, held by nobody.
  /\ e \in retired /\ e \notin freed /\ e \notin Guarded
  \* Freed.
  /\ freed' = freed \cup {e}
  \* Nothing else changes.
  /\ UNCHANGED <<extVars, cur, nextExt, retired, handed, file, bigRetired, writerVars, readerVars>>

\* The same for an extent submit() handed on: kovan places that batch at
\* once, so this step is taken fairly (see LiveSpec).
FreeHanded(e) ==
  \* Handed on.
  /\ e \in handed
  \* Freed when nobody holds it.
  /\ Free(e)

\* ---------------------------------------------------------------------------
\* The whole system.
\* ---------------------------------------------------------------------------

\* One writer's step.
WriterStep(w) ==
  \* Any of them.
  \/ WLoad(w) \/ WState(w) \/ Claim(w) \/ Reserve(w) \/ Zero(w) \/ Fill(w) \/ Publish(w)
  \* Or the copy's and the end's.
  \/ Build(w) \/ Freeze(w) \/ Swap(w) \/ Cap(w) \/ Release(w) \/ LetGo(w) \/ Submit(w) \/ Drop(w)

\* One reader's step.
ReaderStep(r) == RStart(r) \/ RLoad(r) \/ RLen(r) \/ RUnguard(r) \/ RCopy(r) \/ REnd(r)

\* Every step anyone can take.
Next ==
  \* A writer's.
  \/ \E w \in Writers : WriterStep(w)
  \* A reader's.
  \/ \E r \in Readers : ReaderStep(r)
  \* The reclaimer's.
  \/ \E e \in Exts : Free(e)

\* The behaviours: start in Init, take Next steps.
Spec == Init /\ [][Next]_vars

\* The behaviours in which every thread that can go on does go on, and an
\* extent handed to the reclaimer is freed once nobody holds it.
LiveSpec ==
  \* The same steps,
  /\ Spec
  \* every writer's taken,
  /\ \A w \in Writers : WF_vars(WriterStep(w))
  \* every reader's taken,
  /\ \A r \in Readers : WF_vars(ReaderStep(r))
  \* and the prompt reclamation taken.
  /\ \A e \in Exts : WF_vars(FreeHanded(e))

\* ===========================================================================
\* THE RULES
\* ===========================================================================

\* The current extent's published bytes are the file: every write that
\* landed, nothing uninitialized, nothing past the chain. Rules out: an
\* extension that exposes room it never zeroed; a length published before
\* the segment it reaches into is linked; bytes below a published length
\* changed in place.
CurrentIsFile ==
  \* Byte for byte.
  Content(cur) = file

\* Every extent not freed holds initialized, reachable bytes below its
\* published length: a reader still holding a replaced one copies real
\* bytes too.
PublishedInitialized ==
  \* Every extent made and not freed.
  \A e \in Exts : e < nextExt /\ e \notin freed =>
    \* Holds no Uninit and no Gone below its length.
    \A i \in 1..elen[e] : ByteAt(e, i - 1) \notin {Uninit, Gone}

\* A read returns the whole file as it stood at some moment of the read.
\* Rules out: bytes past the length, bytes not yet written, half old and
\* half new.
ReadIsSnapshot ==
  \* Every finished read.
  \A r \in Readers : rpc[r] = "done" => rgot[r] \in rseen[r]

\* The steps at which a writer reads or writes its extent's memory.
WUses == {"state", "claim", "reserve", "zero", "fill", "publish", "build", "freeze", "cap"}

\* No thread is about to use a freed extent. Rules out: a reader or a
\* writer that let go of an extent and then reads it, after kovan freed it.
NoUseAfterFree ==
  \* No writer,
  /\ \A w \in Writers : wpc[w] \in WUses => wext[w] \notin freed
  \* and no reader.
  /\ \A r \in Readers : rpc[r] \in {"len", "copy"} => rext[r] \notin freed

\* A freed extent is not the current one and no guard holds it.
FreedUnreachable ==
  \* Neither the Atom nor a guard.
  freed \cap ({cur} \cup Guarded) = {}

\* Every extent a writer found large when it replaced it is freed in the
\* end. Rules out: a large extent left in a batch that is never placed,
\* holding its memory for as long as the database runs.
LargeFreed ==
  \* For each extent.
  \A e \in Exts : e \in bigRetired ~> e \in freed

\* ===========================================================================
\* WITNESSES: each must break in a GREEN run, which shows the run reaches
\* the race the rules are about.
\* ===========================================================================

\* "No reader copies from an extent a writer is growing in place."
NeverReadsGrowing ==
  \* No reader at its copy ...
  ~\E r \in Readers, w \in Writers :
     \* ... of the extent a writer holds WRITER on after linking a segment.
     /\ rpc[r] = "copy" /\ wpc[w] \in {"zero", "fill", "publish"}
     \* (the same extent)
     /\ wext[w] = rext[r] /\ Len(segs[rext[r]]) > 1

\* "No reader copies from an extent a copy already replaced."
NeverReadsRetired ==
  \* No reader at its copy of a retired extent.
  ~\E r \in Readers : rpc[r] = "copy" /\ rext[r] \in retired

====
