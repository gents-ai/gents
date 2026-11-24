---- MODULE SnapshotRegistry ----
\* THE STORY.
\* A database keeps old versions of a key while some reader still needs them.
\* A reader that wants a stable view takes a "snapshot": it writes down the
\* newest commit number it may see (the horizon) and promises to read only
\* versions at or below it. A background "compaction" throws away versions no
\* reader can see. To know what is safe it must find every snapshot that is
\* alive. If it misses one, it may throw away exactly the version that
\* snapshot still reads, and the reader gets a wrong answer.
\*
\* Tiny example: the horizon is 0 and key k holds version v0. Reader A reads
\* horizon 0. A writer commits v1, so the horizon becomes 1. A compaction
\* looks for live snapshots, finds none yet, and since everyone may now see
\* v1 it deletes v0. Reader A finishes registering at 0 and reads k: v0 is
\* gone. The protocol below stops that.
\*
\* WHAT THIS MODELS (src/engine/snapshot_registry.rs and
\* src/engine/snapshot_registry/chain.rs). There
\* is no lock. Every thread number has a slot; a slot has entries; an entry
\* holds one sequence and a count of the pins on it. A reader registers in
\* its own thread's slot in three moves:
\*   announce  read the horizon h, then put h in an entry: join an entry of
\*             the slot that already holds h (one more count), or claim a
\*             free one;
\*   sample    a SeqCst fence, then read the horizon again;
\*   confirm   still h: the snapshot is live at h. Otherwise take the count
\*             back and announce the new value.
\* A compaction samples (a SeqCst fence) once its inputs are fixed, then
\* reads every entry; its list is every sequence it saw. A copy of a live
\* snapshot adds a count to the original's entry. A release takes one count
\* off the entry the handle recorded, on whatever thread drops the handle.
\* Several threads may share a slot (more threads than slots, or a number
\* handed back and reused), so every entry change is one atomic step.
\*
\* WHAT IS CHECKED, for every interleaving of two handles on two threads
\* (MC_SnapshotRegistry_Green) and of three handles on one thread
\* (MC_SnapshotRegistry_Green_ThreeHandles) over two slots of one entry,
\* with a commit (MaxSeq 1 in the configs) and a compaction that runs again
\* and again. That keeps each configuration to a few seconds on one TLC
\* worker; with three handles on two threads and two entries
\* per slot the green setting was 1,262,916 distinct states (no error), and
\* with MaxSeq 2 as well it was checked once on 16 TLC workers (2026-10-09:
\* 14,454,602 distinct states, no error). Every RED below also fails at
\* those sizes.
\*   MinBelowLive     the minimum the compaction uses is at most every live
\*                    snapshot;
\*   LiveCovered      every live snapshot is in the list or at or above the
\*                    sampled horizon (what stripe compaction needs);
\*   LiveEntryExact   a live snapshot's entry holds exactly its sequence;
\*   PinsExact        an entry's count is exactly the handles recorded there;
\*   ListFromPins     the list holds only sequences some pin held during the
\*                    scan: the documented slack, and nothing else.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/SnapshotRegistry.lean:
\*   scan_respects_live, clone_respects_live, pins_exact, and the RED cases
\*   no_confirm_breaks_min, fresh_clone_breaks_cover and
\*   release_elsewhere_breaks_count as counterexamples.
\*
\* WHAT THE STEPS LEAVE OUT, and why nothing is lost.
\*   - A claim is a compare-and-swap on the taken bits, then the sequence,
\*     then the count. Until the count lands nobody can join the entry (its
\*     count is 0) or claim it (its bit is set), and a scan skips it, so the
\*     claim is one step at the moment the count lands. A release that frees
\*     an entry drops the count, then the bit; the same argument makes it one
\*     step at the count.
\*   - A join looks at an entry (count >= 1, sequence h), then adds its count,
\*     then reads the sequence again. The entry may be freed and claimed for
\*     another sequence between the look and the add: those are three steps
\*     here, so that race is in the model.
\*   - The code only tries to join the slot's last entry; here a join may try
\*     any entry of the slot, which only adds behaviours.
\*   - Memory ordering is sequentially consistent here: the two SeqCst fences
\*     make the real accesses behave so (loom checks that, with
\*     calibrations: tests/loom_snapshots.rs).
\*   - The drain waiter's wake is loom's job, not this model's.
\*
\* CONFIGURATIONS.
\*   MC_SnapshotRegistry_Green                every invariant holds
\*   MC_SnapshotRegistry_Red_NoConfirm        live at the announce: MinBelowLive
\*   MC_SnapshotRegistry_Red_ScanThenSample   scan, then sample: MinBelowLive
\*   MC_SnapshotRegistry_Red_ReleaseHere      a moved handle's release goes to
\*                                            the dropping thread's slot:
\*                                            LiveCovered
\*   MC_SnapshotRegistry_Red_CloneFresh       a copy claims a fresh entry with
\*                                            no confirm: LiveCovered
\*   MC_SnapshotRegistry_Red_PlainClaim       a shared slot is written as if
\*                                            owned: MinBelowLive
\*   MC_SnapshotRegistry_Red_JoinNoRecheck    a join keeps a count on a
\*                                            recycled entry: LiveEntryExact

\* Integers for the sequence numbers, FiniteSets for counting handles, TLC
\* for the symmetry helper `Permutations`.
EXTENDS Integers, FiniteSets, TLC

\* The knobs of a run. Each configuration file (MC_SnapshotRegistry_*.cfg) gives them values.
CONSTANTS
  \* The snapshot handles, as model values. A handle is a `Snapshot` or a
  \* `Transaction` (src/lib.rs, src/transaction.rs); it registers, may be
  \* copied, may move to another thread, and is released.
  Handles,
  \* The threads, as model values. A thread runs registrations in its slot.
  Threads,
  \* The slots, as model values: `SnapshotRegistry::slots`, one per thread
  \* number (src/per_thread.rs).
  Slots,
  \* The entries of one slot, as model values: `chain::Entry`.
  Entries,
  \* "No entry": what a handle that holds no count records.
  NoPos,
  \* The highest the horizon may rise. It bounds the commits.
  MaxSeq,
  \* "Recheck" (the fix) or "None" (RED: go live at the announce).
  Confirm,
  \* "SampleThenScan" (the fix) or "ScanThenSample" (RED).
  Order,
  \* "Recorded" (the fix) or "Here" (RED: release in the dropping thread's
  \* slot, at the recorded entry's index).
  ReleaseMode,
  \* "Join" (the fix) or "Fresh" (RED: a copy claims its own entry).
  CloneMode,
  \* "Cas" (the fix) or "Plain" (RED: see a free entry, write it later).
  ClaimMode,
  \* "Recheck" (the fix) or "NoRecheck" (RED: keep a count on an entry that
  \* was recycled for another sequence).
  JoinMode

\* The horizon is a natural number: it starts at 0 and only goes up.
ASSUME MaxSeq \in Nat
\* Each switch takes one of its two settings.
ASSUME Confirm \in {"Recheck", "None"}
\* As above, for the compaction's order.
ASSUME Order \in {"SampleThenScan", "ScanThenSample"}
\* As above, for where a release goes.
ASSUME ReleaseMode \in {"Recorded", "Here"}
\* As above, for how a copy pins.
ASSUME CloneMode \in {"Join", "Fresh"}
\* As above, for how a claim takes an entry.
ASSUME ClaimMode \in {"Cas", "Plain"}
\* As above, for what a join does after it counted itself.
ASSUME JoinMode \in {"Recheck", "NoRecheck"}

\* Every entry in the registry is named by its slot and its place in it.
Pos == Slots \X Entries

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The newest commit a snapshot may read: `ReadHorizon` in
  \* src/engine/read_horizon.rs.
  horizon,
  \* Which slot each thread announces in: `per_thread::index()`. Two threads
  \* may share one.
  slotOf,
  \* The sequence each entry holds: `Entry::seq`. Only meaningful while its
  \* count is at least 1.
  eseq,
  \* How many pins hold each entry: `Entry::pins`. 0 means free.
  pins,
  \* Where each handle is in its life: "idle" (not registered), "read" (has
  \* read a horizon to announce), "looked" (a join saw a matching entry),
  \* "joining" (a join added its count, not yet rechecked), "seen" (RED plain
  \* claim saw a free entry), "announced" (has a count, not yet confirmed),
  \* "live" (a registered snapshot).
  phase,
  \* The sequence each handle reads, announced or holds.
  hseq,
  \* The entry each handle has its count on, or NoPos: `SnapshotPin`.
  at,
  \* The entry a "looked" join or a "seen" plain claim is about to use.
  saw,
  \* The thread each handle is on now. Handles move between threads.
  thread,
  \* The compaction: "idle", "scanning", "sampling" (RED order only) or
  \* "using" (it has its list and works with it).
  cphase,
  \* The horizon the compaction sampled.
  c,
  \* The entries the current scan has read.
  scanned,
  \* The sequences the current scan saw: its list.
  vals,
  \* The minimum the compaction works with: the least of c and the list.
  m,
  \* Ghost: every sequence some pin held at some moment of this scan. The
  \* real program has no such variable; it states the slack.
  heldDuring

\* Every variable, so a step that changes none of them is a pause (the
\* list goes on to the compaction's variables on the next line).
vars == <<horizon, slotOf, eseq, pins, phase, hseq, at, saw, thread,
          cphase, c, scanned, vals, m, heldDuring>>

\* The least number of a finite set that is not empty.
Min(S) == CHOOSE x \in S : \A y \in S : x <= y

\* Renaming handles, threads, slots or entries changes nothing the checks
\* look at, so TLC checks one state of each renamed family (the second line
\* adds the slot and entry renamings).
Sym == Permutations(Handles) \cup Permutations(Threads)
         \cup Permutations(Slots) \cup Permutations(Entries)

\* The slot handle h announces in: its current thread's slot.
MySlot(h) == slotOf[thread[h]]

\* The sequences held by some pin right now: every entry with a count.
Held == {eseq[p] : p \in {q \in Pos : pins[q] >= 1}}

\* While a compaction runs, a sequence that gains a pin joins the ghost set.
AddHeld(v) == IF cphase = "idle" THEN heldDuring ELSE heldDuring \cup {v}

\* Where a fresh announce leaves the handle: with the fix it must still
\* confirm; RED NoConfirm calls it live at once.
AfterAnnounce == IF Confirm = "None" THEN "live" ELSE "announced"

\* The counts after one count comes off entry p (`Chunk::unpin`).
DropPins(p) == [pins EXCEPT ![p] = @ - 1]

\* The sequences after one count comes off entry p. When it was the last
\* count the entry is free, and the model forgets the old sequence (sets it
\* to 0). The code keeps the stale value, but nothing ever reads a free
\* entry's sequence (a scan and a join both need a count first, and a claim
\* overwrites it), so forgetting it only merges states that behave alike.
DropSeq(p) == IF pins[p] = 1 THEN [eseq EXCEPT ![p] = 0] ELSE eseq

\* The start: horizon 0, every entry free, every handle idle, threads on
\* any slots and handles on any threads (so sharing is covered from the
\* first step), and no compaction running.
Init ==
  \* Nothing is committed yet.
  /\ horizon = 0
  \* Any assignment of threads to slots, shared ones included.
  /\ slotOf \in [Threads -> Slots]
  \* Entries hold sequence 0 but no pin, so they are free.
  /\ eseq = [p \in Pos |-> 0]
  \* No pins anywhere.
  /\ pins = [p \in Pos |-> 0]
  \* No handle is registered.
  /\ phase = [h \in Handles |-> "idle"]
  \* Nothing read yet.
  /\ hseq = [h \in Handles |-> 0]
  \* No handle holds a count.
  /\ at = [h \in Handles |-> NoPos]
  \* No join or plain claim in progress.
  /\ saw = [h \in Handles |-> NoPos]
  \* Handles start on any thread.
  /\ thread \in [Handles -> Threads]
  \* The compaction waits.
  /\ cphase = "idle"
  \* Its sample is unset.
  /\ c = 0
  \* It has read nothing.
  /\ scanned = {}
  \* Its list is empty.
  /\ vals = {}
  \* Its minimum is unset.
  /\ m = 0
  \* The ghost set is empty.
  /\ heldDuring = {}

\* A commit publishes: the horizon goes up by one.
Advance ==
  \* Only up to the bound, so the model is finite.
  /\ horizon < MaxSeq
  \* One more commit is visible.
  /\ horizon' = horizon + 1
  \* Nothing else moves.
  /\ UNCHANGED <<slotOf, eseq, pins, phase, hseq, at, saw, thread,
                 cphase, c, scanned, vals, m, heldDuring>>

\* A handle starts registering: it reads the horizon (`pin_in`'s first load).
Read(h) ==
  \* Only a handle that holds nothing.
  /\ phase[h] = "idle"
  \* It remembers the horizon it read.
  /\ hseq' = [hseq EXCEPT ![h] = horizon]
  \* Next it announces.
  /\ phase' = [phase EXCEPT ![h] = "read"]
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, pins, at, saw, thread,
                 cphase, c, scanned, vals, m, heldDuring>>

\* Announce by claiming a free entry in one compare-and-swap
\* (`Chunk::claim`): the entry gets the sequence and a count of 1.
Claim(h) ==
  \* The fixed claim; RED PlainClaim uses ClaimSee and ClaimWrite instead.
  /\ ClaimMode = "Cas"
  \* The handle has read the horizon and announces it now.
  /\ phase[h] = "read"
  \* Some entry of its thread's slot is free at this very moment.
  /\ \E e \in Entries :
       \* That entry, named by slot and place.
       LET p == <<MySlot(h), e>> IN
         \* Free: no pin holds it.
         /\ pins[p] = 0
         \* It now holds the handle's sequence.
         /\ eseq' = [eseq EXCEPT ![p] = hseq[h]]
         \* With one count: the handle's.
         /\ pins' = [pins EXCEPT ![p] = 1]
         \* The handle records where its count is.
         /\ at' = [at EXCEPT ![h] = p]
  \* Next it confirms (or, in RED NoConfirm, it is live already).
  /\ phase' = [phase EXCEPT ![h] = AfterAnnounce]
  \* A running compaction notes that this sequence was held.
  /\ heldDuring' = AddHeld(hseq[h])
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, hseq, saw, thread, cphase, c, scanned, vals, m>>

\* RED PlainClaim, first half: look for a free entry as if the slot were
\* this thread's alone.
ClaimSee(h) ==
  \* Only in the RED setting.
  /\ ClaimMode = "Plain"
  \* The handle has read the horizon.
  /\ phase[h] = "read"
  \* It picks an entry that is free right now.
  /\ \E e \in Entries :
       \* That entry has no pins right now...
       /\ pins[<<MySlot(h), e>>] = 0
       \* ...and the handle remembers it, without taking it yet.
       /\ saw' = [saw EXCEPT ![h] = <<MySlot(h), e>>]
  \* It will write that entry in a later step.
  /\ phase' = [phase EXCEPT ![h] = "seen"]
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, pins, hseq, at, thread,
                 cphase, c, scanned, vals, m, heldDuring>>

\* RED PlainClaim, second half: write the entry it saw, even if another
\* thread sharing the slot took it in between.
ClaimWrite(h) ==
  \* The handle saw a free entry earlier.
  /\ phase[h] = "seen"
  \* It overwrites the sequence...
  /\ eseq' = [eseq EXCEPT ![saw[h]] = hseq[h]]
  \* ...and sets the count to 1, wiping anyone else's count.
  /\ pins' = [pins EXCEPT ![saw[h]] = 1]
  \* It records that entry as its own.
  /\ at' = [at EXCEPT ![h] = saw[h]]
  \* It forgets what it saw.
  /\ saw' = [saw EXCEPT ![h] = NoPos]
  \* Next it confirms.
  /\ phase' = [phase EXCEPT ![h] = AfterAnnounce]
  \* A running compaction notes that this sequence was held.
  /\ heldDuring' = AddHeld(hseq[h])
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, hseq, thread, cphase, c, scanned, vals, m>>

\* Announce by joining, first move (`Chunk::join`): look at an entry of the
\* slot that has pins and holds exactly the handle's sequence.
JoinLook(h) ==
  \* The handle has read the horizon.
  /\ phase[h] = "read"
  \* Some entry of its slot is held and holds the same sequence.
  /\ \E e \in Entries :
       \* The entry has at least one pin...
       /\ pins[<<MySlot(h), e>>] >= 1
       \* ...it holds the sequence this handle wants...
       /\ eseq[<<MySlot(h), e>>] = hseq[h]
       \* ...and the handle remembers which entry it looked at.
       /\ saw' = [saw EXCEPT ![h] = <<MySlot(h), e>>]
  \* Next it adds its count.
  /\ phase' = [phase EXCEPT ![h] = "looked"]
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, pins, hseq, at, thread,
                 cphase, c, scanned, vals, m, heldDuring>>

\* Join, second move: the compare-and-swap adds one count if the entry is
\* still held. Its sequence may have changed since the look: the entry may
\* have been freed and claimed again in between.
JoinCount(h) ==
  \* The handle looked at an entry.
  /\ phase[h] = "looked"
  \* That entry still has pins (whoever's they are now).
  /\ pins[saw[h]] >= 1
  \* One more count.
  /\ pins' = [pins EXCEPT ![saw[h]] = @ + 1]
  \* The handle records the entry as where its count is.
  /\ at' = [at EXCEPT ![h] = saw[h]]
  \* It forgets the look.
  /\ saw' = [saw EXCEPT ![h] = NoPos]
  \* Next it rechecks the sequence.
  /\ phase' = [phase EXCEPT ![h] = "joining"]
  \* A running compaction notes the entry's sequence was held.
  /\ heldDuring' = AddHeld(eseq[saw[h]])
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, hseq, thread, cphase, c, scanned, vals, m>>

\* Join, the miss: the entry it looked at went free before the add, so the
\* compare-and-swap cannot add; the handle goes back to choose again.
JoinMiss(h) ==
  \* The handle looked at an entry.
  /\ phase[h] = "looked"
  \* That entry has no pins now.
  /\ pins[saw[h]] = 0
  \* It forgets the look.
  /\ saw' = [saw EXCEPT ![h] = NoPos]
  \* Back to announcing the same sequence.
  /\ phase' = [phase EXCEPT ![h] = "read"]
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, pins, hseq, at, thread,
                 cphase, c, scanned, vals, m, heldDuring>>

\* Join, third move: with its count in, the entry's sequence cannot change,
\* so reading it now tells which sequence was joined. The same one: the
\* join stands. Another one (the entry was recycled): take the count back.
\* RED NoRecheck keeps the count whatever the entry holds.
JoinCheck(h) ==
  \* The handle has added its count.
  /\ phase[h] = "joining"
  \* Does the joined entry hold the handle's sequence? (RED: never asks.)
  /\ IF eseq[at[h]] = hseq[h] \/ JoinMode = "NoRecheck"
       \* The join stands; next the confirm.
       THEN /\ phase' = [phase EXCEPT ![h] = AfterAnnounce]
            \* The counts, the entries' sequences and the handles' places do not change.
            /\ UNCHANGED <<pins, eseq, at>>
       \* Wrong sequence: one count off, the handle holds nothing again...
       ELSE /\ pins' = DropPins(at[h])
            \* (if that was the entry's last count, it is free now)
            /\ eseq' = DropSeq(at[h])
            \* The handle records no entry.
            /\ at' = [at EXCEPT ![h] = NoPos]
            \* ...and announces again.
            /\ phase' = [phase EXCEPT ![h] = "read"]
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, hseq, saw, thread,
                 cphase, c, scanned, vals, m, heldDuring>>

\* Sample and confirm (`SnapshotRegistry::confirm`): read the horizon
\* again after the fence. Unchanged: the snapshot is live at it. Changed:
\* take the count back and announce the new value.
Confirm_(h) ==
  \* The handle has announced.
  /\ phase[h] = "announced"
  \* The second read of the horizon: the same value as announced?
  /\ IF horizon = hseq[h]
       \* Still the same: live. Its count stays where it is.
       THEN /\ phase' = [phase EXCEPT ![h] = "live"]
            \* Nothing else of the registry or of the handle changes.
            /\ UNCHANGED <<pins, eseq, at, hseq>>
       \* Moved on: one count off its entry...
       ELSE /\ pins' = DropPins(at[h])
            \* (if that was the entry's last count, it is free now)
            /\ eseq' = DropSeq(at[h])
            \* ...it holds nothing...
            /\ at' = [at EXCEPT ![h] = NoPos]
            \* ...and announces the value it just read.
            /\ hseq' = [hseq EXCEPT ![h] = horizon]
            \* Back to announcing, with no new read of the horizon needed.
            /\ phase' = [phase EXCEPT ![h] = "read"]
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, saw, thread,
                 cphase, c, scanned, vals, m, heldDuring>>

\* A copy of a live snapshot (`clone_pin`). The fix adds one count to the
\* original's entry, which already holds the sequence, so the copy is
\* covered from the moment the original was. RED Fresh claims a free entry
\* of the copy's own slot and calls it live with no confirm.
Clone(h, g) ==
  \* h is live...
  /\ phase[h] = "live"
  \* ...and g is a handle not in use.
  /\ phase[g] = "idle"
  \* The fix joins; the RED claims afresh.
  /\ IF CloneMode = "Join"
       \* One more count on h's entry...
       THEN /\ pins' = [pins EXCEPT ![at[h]] = @ + 1]
            \* ...recorded in g...
            /\ at' = [at EXCEPT ![g] = at[h]]
            \* ...and the entry's sequence, already h's, stays.
            /\ UNCHANGED eseq
       \* RED: some entry of g's slot...
       ELSE \E e \in Entries :
              \* (named by slot and place)
              LET p == <<MySlot(g), e>> IN
                \* ...that is free...
                /\ pins[p] = 0
                \* ...gets h's sequence...
                /\ eseq' = [eseq EXCEPT ![p] = hseq[h]]
                \* ...and one count...
                /\ pins' = [pins EXCEPT ![p] = 1]
                \* ...recorded in g, with no confirm.
                /\ at' = [at EXCEPT ![g] = p]
  \* g reads at h's sequence.
  /\ hseq' = [hseq EXCEPT ![g] = hseq[h]]
  \* g is live.
  /\ phase' = [phase EXCEPT ![g] = "live"]
  \* A running compaction notes the sequence was held.
  /\ heldDuring' = AddHeld(hseq[h])
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, saw, thread, cphase, c, scanned, vals, m>>

\* A live snapshot is dropped (`SnapshotRegistry::release`). The fix takes
\* one count off the entry the handle recorded, whatever thread it is on.
\* RED Here takes it off the same place in the current thread's slot.
Release(h) ==
  \* Only a live handle is released.
  /\ phase[h] = "live"
  \* The entry the release touches.
  \* The fix: the recorded entry. RED: the same place in this thread's slot.
  /\ LET p == IF ReleaseMode = "Recorded" THEN at[h]
              \* The bug ReleaseHere: the same index, but in the slot of the thread that drops the
              \* handle.
              ELSE <<MySlot(h), at[h][2]>> IN
       \* One count off, if there is one to take (a count never goes below 0).
       IF pins[p] > 0
         \* The count drops by one...
         THEN /\ pins' = DropPins(p)
              \* ...and a last count frees the entry.
              /\ eseq' = DropSeq(p)
         \* Nothing to take: nothing changes.
         ELSE UNCHANGED <<pins, eseq>>
  \* The handle holds nothing.
  /\ at' = [at EXCEPT ![h] = NoPos]
  \* It may register again later. What it read is forgotten: an idle
  \* handle's sequence is never read, so this only merges alike states.
  /\ phase' = [phase EXCEPT ![h] = "idle"]
  \* Its sequence is set back to 0.
  /\ hseq' = [hseq EXCEPT ![h] = 0]
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, saw, thread,
                 cphase, c, scanned, vals, m, heldDuring>>

\* A handle moves to another thread: a `Snapshot` or `Transaction` is
\* `Send`. A registration runs inside one call on one thread, so only an
\* idle or live handle moves.
Move(h, t) ==
  \* Not in the middle of registering.
  /\ phase[h] \in {"idle", "live"}
  \* Now on thread t.
  /\ thread' = [thread EXCEPT ![h] = t]
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, pins, phase, hseq, at, saw,
                 cphase, c, scanned, vals, m, heldDuring>>

\* A thread number changes hands: a thread exits and gives its number back,
\* and a new thread takes it, or takes a shared one. In the model the thread
\* t simply starts announcing in slot s. Its handles' counts stay where they
\* were recorded.
Rehome(t, s) ==
  \* No handle of t is in the middle of registering.
  /\ \A h \in Handles : thread[h] = t => phase[h] \in {"idle", "live"}
  \* t now announces in s.
  /\ slotOf' = [slotOf EXCEPT ![t] = s]
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, eseq, pins, phase, hseq, at, saw, thread,
                 cphase, c, scanned, vals, m, heldDuring>>

\* The compaction samples (`SnapshotRegistry::sample`): it fixes the horizon
\* its inputs are no newer than. The fix does it before the scan; RED
\* ScanThenSample after.
CSample ==
  \* With the fix it starts here, from idle...
  /\ \/ Order = "SampleThenScan" /\ cphase = "idle"
     \* ...in RED order it comes after the scan.
     \/ Order = "ScanThenSample" /\ cphase = "sampling"
  \* It remembers the horizon now.
  /\ c' = horizon
  \* With the fix it scans next; in RED order it has its list already.
  /\ cphase' = IF Order = "SampleThenScan" THEN "scanning" ELSE "using"
  \* In RED order the minimum is fixed now, from the list and the sample.
  /\ m' = IF Order = "SampleThenScan" THEN m ELSE Min({horizon} \cup vals)
  \* With the fix the ghost starts from what is held right now.
  /\ heldDuring' = IF Order = "SampleThenScan" THEN Held ELSE heldDuring
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, pins, phase, hseq, at, saw, thread,
                 scanned, vals>>

\* The compaction reads one entry it has not read in this scan
\* (`Chunk::each_announced`): a held entry adds its sequence to the list.
CScan(p) ==
  \* With the fix only after the sample...
  /\ \/ Order = "SampleThenScan" /\ cphase = "scanning"
     \* ...in RED order from the start.
     \/ Order = "ScanThenSample" /\ cphase \in {"idle", "scanning"}
  \* Each entry once per scan: not read yet...
  /\ p \notin scanned
  \* ...and now read.
  /\ scanned' = scanned \cup {p}
  \* A held entry is listed; a free one is skipped.
  /\ vals' = IF pins[p] >= 1 THEN vals \cup {eseq[p]} ELSE vals
  \* The scan is under way.
  /\ cphase' = "scanning"
  \* In RED order the ghost starts at the first read.
  /\ heldDuring' = IF cphase = "idle" THEN Held ELSE heldDuring
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, pins, phase, hseq, at, saw, thread, c, m>>

\* Every entry has been read. With the fix the minimum is now the least of
\* the sample and the list; in RED order the sample still has to happen.
CScanDone ==
  \* A scan is running...
  /\ cphase = "scanning"
  \* ...and has read every entry.
  /\ scanned = Pos
  \* Use the list, or (RED) go and sample.
  /\ cphase' = IF Order = "SampleThenScan" THEN "using" ELSE "sampling"
  \* The minimum, with the fix.
  /\ m' = IF Order = "SampleThenScan" THEN Min({c} \cup vals) ELSE m
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, pins, phase, hseq, at, saw, thread,
                 c, scanned, vals, heldDuring>>

\* The compaction finishes; a later one may start from scratch.
CFinish ==
  \* It was using its list.
  /\ cphase = "using"
  \* Everything about it is forgotten: it waits again...
  /\ cphase' = "idle"
  \* ...with no sample...
  /\ c' = 0
  \* ...nothing read...
  /\ scanned' = {}
  \* ...an empty list...
  /\ vals' = {}
  \* ...no minimum...
  /\ m' = 0
  \* ...and an empty ghost set.
  /\ heldDuring' = {}
  \* Nothing else moves.
  /\ UNCHANGED <<horizon, slotOf, eseq, pins, phase, hseq, at, saw, thread>>

\* Every step the system can take, one of:
Next ==
  \* A commit.
  \/ Advance
  \* A handle takes one step of registering or releasing:
  \/ \E h \in Handles :
       \* reading the horizon, or claiming an entry (fixed or RED),
       \/ Read(h) \/ Claim(h) \/ ClaimSee(h) \/ ClaimWrite(h)
       \* or one of the three moves of a join, or a join that missed,
       \/ JoinLook(h) \/ JoinCount(h) \/ JoinMiss(h) \/ JoinCheck(h)
       \* or confirming, or being released.
       \/ Confirm_(h) \/ Release(h)
  \* A copy of one handle into another.
  \/ \E h, g \in Handles : h # g /\ Clone(h, g)
  \* A handle moves thread.
  \/ \E h \in Handles, t \in Threads : Move(h, t)
  \* A thread number changes hands.
  \/ \E t \in Threads, s \in Slots : Rehome(t, s)
  \* The compaction samples...
  \/ CSample
  \* ...reads one entry...
  \/ \E p \in Pos : CScan(p)
  \* ...finishes its scan...
  \/ CScanDone
  \* ...or finishes altogether.
  \/ CFinish

\* Every behaviour: start in Init, then take Next steps or pause.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* What must always be true.

\* Every variable holds a value of the kind its comment says.
TypeOK ==
  \* The horizon stays within its bound.
  /\ horizon \in 0..MaxSeq
  \* Every thread has a slot.
  /\ slotOf \in [Threads -> Slots]
  \* Every entry holds a sequence the horizon has reached.
  /\ eseq \in [Pos -> 0..MaxSeq]
  \* No entry has more counts than there are handles.
  /\ pins \in [Pos -> 0..Cardinality(Handles)]
  \* Every handle is in one of the named phases.
  /\ phase \in [Handles -> {"idle", "read", "looked", "joining", "seen",
                            "announced", "live"}]
  \* Every handle's sequence is one the horizon has reached.
  /\ hseq \in [Handles -> 0..MaxSeq]
  \* A handle records an entry or none.
  /\ at \in [Handles -> Pos \cup {NoPos}]
  \* A handle remembers an entry it looked at, or none.
  /\ saw \in [Handles -> Pos \cup {NoPos}]
  \* Every handle is on some thread.
  /\ thread \in [Handles -> Threads]
  \* The compaction is in one of its phases.
  /\ cphase \in {"idle", "scanning", "sampling", "using"}
  \* It has read some of the entries.
  /\ scanned \subseteq Pos
  \* Its list holds sequences the horizon has reached.
  /\ vals \subseteq 0..MaxSeq
  \* So does the ghost set.
  /\ heldDuring \subseteq 0..MaxSeq

\* The handles holding a live snapshot.
Live == {h \in Handles : phase[h] = "live"}

\* THE HEADLINE. While the compaction works with its minimum, the minimum
\* is at most every live snapshot, including one that became live after the
\* scan. It rules out: reader at 0, compaction minimum 1, version at 0
\* thrown away. Lean: scan_respects_live.
MinBelowLive == cphase = "using" => \A h \in Live : m <= hseq[h]

\* What stripe compaction needs: a live snapshot is in the list (a stripe
\* ends at it) or at or above the sample (it reads the top stripe of inputs
\* fixed before the sample). It rules out a snapshot that is neither, whose
\* version would be folded away. Lean: scan_respects_live,
\* clone_respects_live.
LiveCovered == cphase = "using" => \A h \in Live : hseq[h] \in vals \/ c <= hseq[h]

\* A live snapshot's entry holds its exact sequence and a count. It rules
\* out an entry that says 2 for a snapshot reading at 1: a scan would cut a
\* stripe at the wrong place.
LiveEntryExact == \A h \in Live : eseq[at[h]] = hseq[h] /\ pins[at[h]] >= 1

\* An entry's count is exactly the handles recorded at it: no release lost,
\* none counted twice, a moved handle's included. It rules out an entry
\* freed while a handle still records it. Lean: pins_exact.
PinsExact == \A p \in Pos : pins[p] = Cardinality({h \in Handles : at[h] = p})

\* The slack, stated exactly: everything the scan listed was held by some
\* pin at some moment of the scan. The list may hold a sequence released
\* during the scan, or one a registration is about to take back; nothing
\* else.
ListFromPins == cphase # "idle" => vals \subseteq heldDuring

====
