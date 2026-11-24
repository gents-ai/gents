---- MODULE ClaimSlots ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A transaction that has something to run when it aborts (an on_abort
\* callback, or a database with transaction hooks) is listed with the engine,
\* so that `close` can find it and abort it. The list is a table of slots
\* (ClaimSlots, src/engine/open_transactions/claim_slots.rs):
\*
\*   - a transaction takes a free slot when it begins and puts a reference
\*     to its claim in it (reserve, then fill);
\*   - it empties the slot and gives it back when it ends (remove);
\*   - close takes every claim still listed, emptying each slot (drain).
\*
\* The free slots sit on a stack. Each free slot names the slot below it
\* (`below`), and one word (`free`) holds the top slot and a count of every
\* change ever made to the stack (the "tag"). Taking a slot is one
\* compare-and-swap (CAS) of that word from "top A" to "top below[A]";
\* giving one back is one CAS from "top X" to "top me" after writing
\* below[me] = X. A new slot is made only when the stack is empty, by
\* counting `taken` up; the table grows in chunks, made the first time a
\* slot in them is used, and never shrinks.
\*
\* Whoever swaps the reference out of a slot owns it: the transaction at its
\* end, or close when it gets there first. Exactly one of them drops it.
\*
\* Tiny example of why the tag is there (the "ABA" problem). Slots A and B
\* are free, A on top, B below it. Thread 1 starts to take a slot: it reads
\* "top A" and below[A] = B, and is paused. Thread 2 takes A, takes B, and
\* gives A back: the stack is "top A" again, with nothing below A now.
\* Without the tag, thread 1's CAS from "top A" to "top B" succeeds, and B,
\* which thread 2 holds, is on the stack: the next taker gets B too, and two
\* transactions share one slot. With the tag, the word thread 1 read
\* ("top A, change 0") is not the word now ("top A, change 3"): its CAS
\* fails, and it reads the stack again.
\*
\* What this model checks cannot happen:
\*   - a slot held by two transactions at once (OneHolder);
\*   - a free stack that loses a slot, holds one twice, loops, or holds a
\*     slot some transaction holds (FreeStackSound);
\*   - a slot whose reference is not the claim of the transaction holding
\*     it (CellBelongsToHolder);
\*   - a claim's reference dropped twice, or by anyone but its owner or
\*     close (DroppedOnce);
\*   - close finishing while a claim listed when it began is still listed
\*     (LiveTakenByClose);
\*   - close reading past the table (DrainInBounds);
\*   - once everybody is done: a claim never ended, a slot still holding a
\*     reference, a reference never dropped, a slot never given back
\*     (AtRest). A claim close missed, or one begun after close was refused
\*     a slot, is ended by its owner: none is lost.
\*
\* WHAT IS MODELLED, and the code each piece mirrors (file and line of the
\* tree the model was checked against; claim_slots.rs unless named):
\*   free          ClaimSlots::free (58): `top` is the low half (the slot
\*                 plus one in Rust, so slots are numbered from 1 here and
\*                 0 is "empty"), `tag` the high half.
\*   below         Chunk::below (38).
\*   taken         ClaimSlots::taken (60).
\*   chunks        the chunks whose cell is set: ClaimSlots::chunks (54), an
\*                 array of crate::sync::OnceCell. ClaimSlots::chunk (120-126)
\*                 sets one with get_or_init_racy (124): threads that race
\*                 each build a chunk, one compare-and-swap publishes one of
\*                 them and the others are dropped unseen, so the cell goes
\*                 from unset to set in one atomic step, as Init does here.
\*                 drain (206-208) and take (227-229) only look with get.
\*   cell          Chunk::claims (36): a claim's id, or None for null.
\*   closed        RegolithEngine::close_state != OPEN, read by
\*                 RegolithEngine::is_closed (engine/mod.rs:1165-1167) in
\*                 Claim::track (transaction/claim.rs:57).
\*   cstate        Claim::state (transaction/claim.rs:41): open, committing
\*                 or aborted; "none" before the transaction begins.
\*   slotOf        Claim::slot (transaction/claim.rs:40), 0 when not listed;
\*                 set here when the slot is taken, which is when the
\*                 transaction holds it.
\*   given, dropBy ghosts: the slot went back on the stack; who dropped the
\*                 reference the slot held (an owner's number, or 0 for
\*                 close).
\*   pc..held      each owner's step and its locals (`head` and `next` of
\*                 take and remove).
\*   cpc..chand    the closer's step and its locals; `live` is a ghost, the
\*                 claims listed when the drain began.
\*
\*   Model step   Rust
\*   Check        Claim::track: `!engine.is_closed()` (transaction/claim.rs:57)
\*   Pop          take: `self.free.load(Acquire)` (222)
\*   Below        take: `chunk.below[at].load(Relaxed)`, 0 when the chunk is
\*                not there (`map_or(0, ..)`, 227-229)
\*   PopCas       take: next_word and the compare_exchange_weak (230-237); a
\*                failure carries the word it found, as `Err(current)` does
\*   Fresh        take: `self.taken.fetch_add(1, AcqRel)` and the CAPACITY
\*                check (239-240); reserve's None (131-133)
\*   Init         fill: `self.chunk(slot as usize)` (137), the chunk's
\*                get_or_init_racy (124)
\*   Fill         fill: the claim stored with Release through store_ptr
\*                (138-148)
\*   Commit       Claim::begin_commit, OPEN -> COMMITTING
\*                (transaction/claim.rs:90), before release_resources
\*                (transaction.rs:1983-1993)
\*   Swap         remove: `drop(self.take_claim(..))` (162), take_claim's
\*                swap (256)
\*   PushLoad     give_back: `self.free.load(Relaxed)` (174)
\*   PushBelow    give_back: `chunk.below[at].store(..)` (176)
\*   PushCas      give_back: next_word and the compare_exchange_weak
\*                (177-184)
\*   Abort        Claim::owner_abort after release_resources on rollback
\*                (transaction/claim.rs:102, transaction.rs:2065-2068)
\*   Close        close: `close_state.store(CLOSE_STATE_CLOSING)`
\*                (engine/mod.rs:3372-3373)
\*   Bound        drain: `self.taken.load(Acquire).min(CAPACITY)` (204)
\*   Visit        drain: the chunk's get, take_claim's swap (205-215)
\*   CloseAbort   Claim::abort_for_close (transaction/claim.rs:144), whose
\*                CAS is Claim::win's (transaction/claim.rs:82-86)
\*
\*   Rule                Rust it holds for
\*   OneHolder           reserve hands a slot to one caller (131-133, 221-241)
\*   FreeStackSound      take and give_back (the tag: next_word, 190-196)
\*   CellBelongsToHolder remove empties before it gives back (162 before 163)
\*   DroppedOnce         take_claim's swap (243-264)
\*   LiveTakenByClose    drain looks at every slot below `taken` (204)
\*   DrainInBounds       drain's `.min(CAPACITY)` (204)
\*   AtRest              OpenTransactions::abort_all with every owner's end
\*                       (engine/open_transactions.rs:68)
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - The chunk sizes: chunk k holds 2^k slots here (FIRST = 1), against
\*     64 << k in Rust. What matters here is that a slot's chunk may be made
\*     while close looks at it, which a chunk of any size shows. `place`,
\*     which finds a slot's chunk and offset, is proved for every size in
\*     proofs/lean/Regolith/ClaimSlots.lean (place_bijective): every slot
\*     below CAPACITY lands in a cell that exists, no two slots share a
\*     cell, and every cell is some slot's; with the real constants every
\*     slot plus one fits the u32 the free stack links by (real_slot_fits).
\*   - Memory ordering: every step here is atomic and in one order. The
\*     orderings that make them so (Acquire, Release, AcqRel) are checked by
\*     the loom models in src/engine/open_transactions/loom_model.rs, on the
\*     real ClaimSlots.
\*   - The tag's width: 32 bits in Rust, wrapping after 2^32 changes made
\*     while one thread is paused between its read and its CAS. A run here
\*     makes a few changes, never that many. That any number of changes
\*     from 1 to 2^32 - 1, whatever tops they leave, makes the word differ
\*     from the one read before them is proved in
\*     proofs/lean/Regolith/ClaimSlots.lean (no_aba), on the word's bits as
\*     next_word writes them (nextWord_eq).
\*   - What a claim is and what aborting one runs: TxnCallbacks.tla. Here a
\*     claim is an id and its state word.
\*   - A transaction whose commit began before it had a claim
\*     (`committing`, transaction/claim.rs:57): it is never listed.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one rule it
\* names):
\*   MC_ClaimSlots_Green              two owners, one transaction each, the
\*                                    table growing from nothing into its
\*                                    second chunk, close racing both.
\*   MC_ClaimSlots_Green_Reuse        one owner runs two transactions in a
\*                                    row, so a slot given back is taken
\*                                    again while close drains.
\*   MC_ClaimSlots_Green_ReuseShared  the same with a second owner and no
\*                                    close: either owner takes a slot the
\*                                    other gave back. (Both owners and
\*                                    close at once is over the time bound;
\*                                    the drain only reads the slots, so the
\*                                    two runs together cover it.)
\*   MC_ClaimSlots_Green_ABA          two free slots; one owner holds two
\*                                    transactions, gives one back and
\*                                    begins a third while the other owner
\*                                    takes a slot: the ABA race, with the
\*                                    tag.
\*   MC_ClaimSlots_Green_Full         a table of one slot and two owners:
\*                                    one is refused a slot, `taken` passes
\*                                    the capacity, and close stays inside.
\*   MC_ClaimSlots_Red_NoTag          the ABA race without the tag:
\*                                    FreeStackSound breaks.
\*   MC_ClaimSlots_Red_GiveBackFirst  remove gives the slot back before it
\*                                    empties it: CellBelongsToHolder breaks.
\*   MC_ClaimSlots_Red_TakeByLoad     a slot is emptied by a load and then a
\*                                    store, not one swap: DroppedOnce
\*                                    breaks.
\*   MC_ClaimSlots_Red_NoClamp        drain looks at `taken` slots without
\*                                    the capacity: DrainInBounds breaks.
\*   MC_ClaimSlots_Red_StopAtEmpty    drain stops at the first empty slot:
\*                                    LiveTakenByClose breaks.
\*   MC_ClaimSlots_Green_ReachesABA   MC_ClaimSlots_Green_ABA with the
\*                                    witness NeverABA, which must break:
\*                                    the GREEN run reaches the ABA race.
\*   MC_ClaimSlots_Green_ReachesCloseFirst
\*                                    MC_ClaimSlots_Green with NeverCloseFirst:
\*                                    close aborts a transaction whose owner
\*                                    then tries to commit.
\*   MC_ClaimSlots_Green_ReachesLateChunk
\*                                    MC_ClaimSlots_Green with NeverLateChunk:
\*                                    close looks at a slot whose chunk is
\*                                    still being made.

\* Numbers, sequences, and the size of a set.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The threads that run transactions, numbered from 1.
  Owners,
  \* [Owners -> Seq({"begin", "end"})]: what each owner does, in order:
  \* begin a transaction, or end its oldest one still open.
  Script,
  \* How many chunks the table may hold (CHUNKS); chunk k holds 2^k slots.
  Chunks,
  \* Slots handed out and given back before the run: the stack holds them.
  PreFree,
  \* Whether close runs.
  Closes,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The bug names this model knows.
ASSUME Mutant \in {"none", "NoTag", "GiveBackFirst", "TakeByLoad", "NoClamp", "StopAtEmpty"}
\* Owners are numbered from 1: 0 stands for close.
ASSUME Owners \subseteq Nat \ {0}
\* At least one chunk.
ASSUME Chunks \in Nat \ {0}
\* Close runs or not.
ASSUME Closes \in BOOLEAN

\* Every slot the chunks hold (CAPACITY).
Cap == 2 ^ Chunks - 1
\* The slots, numbered from 1.
Slots == 1..Cap
\* The pre-grown slots fit in the table.
ASSUME PreFree \in 0..Cap
\* How many transactions owner o begins.
Begins(o) == Cardinality({i \in DOMAIN Script[o] : Script[o][i] = "begin"})
\* Every transaction: owner o's k-th.
Tx == UNION {{<<o, k>> : k \in 1..Begins(o)} : o \in Owners}
\* An empty slot (a null pointer).
None == <<0, 0>>
\* The chunk slot s is in: chunk k holds slots 2^k to 2^(k+1) - 1.
ChunkOf(s) == CHOOSE k \in 0..(Chunks - 1) : 2 ^ k <= s /\ s < 2 ^ (k + 1)
\* The smaller of two numbers.
Min(a, b) == IF a < b THEN a ELSE b
\* The tag a successful CAS writes: one more change, or none without it.
NextTag(t) == IF Mutant = "NoTag" THEN t ELSE t + 1

\* For the configurations: owners 1 and 2 each begin one transaction and
\* end it.
OneEach == [o \in {1, 2} |-> <<"begin", "end">>]
\* Owner 1 runs two transactions one after the other; owner 2 runs one.
TwoInARow == [o \in {1, 2} |-> IF o = 1 THEN <<"begin", "end", "begin", "end">>
                                        ELSE <<"begin", "end">>]
\* Owner 1 runs two transactions one after the other; owner 2 none.
TwoInARowAlone == [o \in {1, 2} |-> IF o = 1 THEN <<"begin", "end", "begin", "end">>
                                             ELSE <<>>]
\* The ABA race: owner 1 takes a slot and gives it back; owner 2 takes two,
\* gives the first back, and takes a third before it ends the other two.
ABA == [o \in {1, 2} |-> IF o = 1 THEN <<"begin", "end">>
                                  ELSE <<"begin", "begin", "end", "begin", "end", "end">>]

\* The state that changes from step to step.
VARIABLES
  \* [tag, top]: the free stack's word.
  free,
  \* [Slots -> 0..Cap]: for a free slot, the slot below it (0: none).
  below,
  \* How many new slots were ever handed out (it may pass Cap).
  taken,
  \* The chunks made so far.
  chunks,
  \* [Slots -> Tx \cup {None}]: the claim each slot holds a reference to.
  cell,
  \* Whether close has begun.
  closed,
  \* [Tx -> state]: each claim's state word.
  cstate,
  \* [Tx -> 0..Cap]: the slot each transaction took (0: none).
  slotOf,
  \* [Tx -> BOOLEAN]: its slot went back on the stack.
  given,
  \* [Tx -> SUBSET (Owners \cup {0})]: who dropped the slot's reference.
  dropBy,
  \* [Owners -> step]: where each owner is.
  pc,
  \* [Owners -> Nat]: the next entry of its script.
  ip,
  \* [Owners -> Seq(Tx)]: its transactions still open, oldest first.
  open,
  \* [Owners -> Nat]: how many transactions it has begun.
  nbegun,
  \* [Owners -> Tx]: the transaction it is beginning or ending.
  tx,
  \* [Owners -> [tag, top]]: the free word it last read.
  head,
  \* [Owners -> 0..Cap]: the `below` it read for that top.
  next,
  \* [Owners -> {"commit", "rollback"}]: how it ends the transaction.
  path,
  \* [Owners -> Tx \cup {None}]: what a load-then-store read (TakeByLoad).
  held,
  \* The closer's step.
  cpc,
  \* How many slots the drain looks at.
  bound,
  \* The next slot the drain looks at.
  ci,
  \* The claim the drain took and is aborting, or read (TakeByLoad).
  chand,
  \* Ghost: the claims listed when the drain began.
  live,
  \* Whether the drain indexed past the table (a panic in Rust).
  panicked

\* The table's variables.
tableVars == <<free, below, taken, chunks, cell>>
\* The claims' variables.
claimVars == <<closed, cstate, slotOf, given, dropBy>>
\* The owners' variables.
ownerVars == <<pc, ip, open, nbegun, tx, head, next, path, held>>
\* The closer's variables.
closeVars == <<cpc, bound, ci, chand, live, panicked>>
\* Every variable.
vars == <<tableVars, claimVars, ownerVars, closeVars>>

\* The steps an owner takes, by name.
OwnerSteps == {"idle", "check", "pop", "below", "popcas", "fresh", "init", "fill",
               "commit", "swap", "store", "pushload", "pushbelow", "pushcas", "abort", "finish"}

\* Every variable holds the kind of value it should.
TypeOK ==
  \* The word: a tag and a top slot.
  /\ free \in [tag : Nat, top : 0..Cap]
  \* Every slot's below names a slot or none.
  /\ below \in [Slots -> 0..Cap]
  \* A count.
  /\ taken \in Nat
  \* Chunk numbers.
  /\ chunks \subseteq 0..(Chunks - 1)
  \* Each slot holds a claim or nothing.
  /\ cell \in [Slots -> Tx \cup {None}]
  \* Close has begun or not.
  /\ closed \in BOOLEAN
  \* Each claim's state.
  /\ cstate \in [Tx -> {"none", "open", "committing", "aborted"}]
  \* Each transaction's slot.
  /\ slotOf \in [Tx -> 0..Cap]
  \* Given back or not.
  /\ given \in [Tx -> BOOLEAN]
  \* Who dropped each reference.
  /\ dropBy \in [Tx -> SUBSET (Owners \cup {0})]
  \* Each owner is at one of its steps.
  /\ pc \in [Owners -> OwnerSteps]
  \* The closer is at one of its steps.
  /\ cpc \in {"idle", "bound", "visit", "store", "abort", "done"}

\* The start: PreFree slots on the stack (1 on top), nobody has done
\* anything.
Init ==
  \* Slot 1 on top when there are free slots; the tag starts at 0.
  /\ free = [tag |-> 0, top |-> IF PreFree > 0 THEN 1 ELSE 0]
  \* Free slot s has s + 1 below it; the last has none.
  /\ below = [s \in Slots |-> IF s < PreFree THEN s + 1 ELSE 0]
  \* The pre-grown slots were handed out once.
  /\ taken = PreFree
  \* Their chunks exist.
  /\ chunks = {ChunkOf(s) : s \in 1..PreFree}
  \* No slot holds a claim.
  /\ cell = [s \in Slots |-> None]
  \* The database is open.
  /\ closed = FALSE
  \* No transaction has begun.
  /\ cstate = [t \in Tx |-> "none"]
  \* None holds a slot.
  /\ slotOf = [t \in Tx |-> 0]
  \* None gave one back.
  /\ given = [t \in Tx |-> FALSE]
  \* No reference was dropped.
  /\ dropBy = [t \in Tx |-> {}]
  \* Every owner is about to read its script.
  /\ pc = [o \in Owners |-> "idle"]
  \* From the first entry.
  /\ ip = [o \in Owners |-> 1]
  \* With nothing open.
  /\ open = [o \in Owners |-> <<>>]
  \* Having begun nothing.
  /\ nbegun = [o \in Owners |-> 0]
  \* Its current transaction is a placeholder.
  /\ tx = [o \in Owners |-> None]
  \* It has read no word.
  /\ head = [o \in Owners |-> [tag |-> 0, top |-> 0]]
  \* Nor any below.
  /\ next = [o \in Owners |-> 0]
  \* Nor chosen how to end.
  /\ path = [o \in Owners |-> "commit"]
  \* Nor read a slot.
  /\ held = [o \in Owners |-> None]
  \* Close has not begun.
  /\ cpc = "idle"
  \* It looks at no slot yet.
  /\ bound = 0
  \* From slot 1.
  /\ ci = 1
  \* It holds no claim.
  /\ chand = None
  \* No claim noted.
  /\ live = {}
  \* No panic.
  /\ panicked = FALSE

\* ---------------------------------------------------------------------------
\* Owners.
\* ---------------------------------------------------------------------------

\* Owner o's transaction t, with its slot.
SlotOfTx(o) == slotOf[tx[o]]

\* The step after a slot is emptied and given back: a rollback then ends
\* its claim (owner_abort), a commit already did.
AfterRemove(o) == IF path[o] = "rollback" THEN "abort" ELSE "finish"
\* The first step of remove: the swap, or (GiveBackFirst) the push.
FirstRemoveStep == IF Mutant = "GiveBackFirst" THEN "pushload" ELSE "swap"
\* The step after the swap.
AfterSwap(o) == IF Mutant = "GiveBackFirst" THEN AfterRemove(o) ELSE "pushload"
\* The step after the push.
AfterPush(o) == IF Mutant = "GiveBackFirst" THEN "swap" ELSE AfterRemove(o)

\* An owner reads the next entry of its script: begin a transaction, or end
\* its oldest open one by commit or by rollback.
Start(o) ==
  \* It is between transactions' steps, with script left.
  /\ pc[o] = "idle"
  \* (Script left.)
  /\ ip[o] <= Len(Script[o])
  \* Begin: a new claim, owner o's next.
  /\ IF Script[o][ip[o]] = "begin"
     \* Its id is owner o's next number.
     THEN /\ tx' = [tx EXCEPT ![o] = <<o, nbegun[o] + 1>>]
          \* Counted as begun.
          /\ nbegun' = [nbegun EXCEPT ![o] = nbegun[o] + 1]
          \* It goes to Claim::track's check.
          /\ pc' = [pc EXCEPT ![o] = "check"]
          \* No path yet.
          /\ UNCHANGED path
     \* End: its oldest open transaction, by commit or rollback.
     ELSE /\ open[o] # <<>>
          \* That transaction.
          /\ tx' = [tx EXCEPT ![o] = Head(open[o])]
          \* Either way of ending.
          /\ \E p \in {"commit", "rollback"} :
               \* The path chosen.
               /\ path' = [path EXCEPT ![o] = p]
               \* A commit claims the state word first; a rollback empties
               \* its slot first, or aborts straight away when it has none.
               /\ pc' = [pc EXCEPT ![o] =
                          IF p = "commit" THEN "commit"
                          ELSE IF slotOf[Head(open[o])] # 0 THEN FirstRemoveStep ELSE "abort"]
          \* The count stays.
          /\ UNCHANGED nbegun
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, claimVars, ip, open, head, next, held, closeVars>>

\* Claim::track: a database that has begun to close lists nothing; else
\* the transaction takes a slot.
Check(o) ==
  \* About to check.
  /\ pc[o] = "check"
  \* The claim exists from here, open.
  /\ cstate' = [cstate EXCEPT ![tx[o]] = "open"]
  \* Closed: not listed, the owner ends it itself. Open: take a slot.
  /\ pc' = [pc EXCEPT ![o] = IF closed THEN "finish" ELSE "pop"]
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, closed, slotOf, given, dropBy, ip, open, nbegun, tx, head, next,
                 path, held, closeVars>>

\* take: read the free word.
Pop(o) ==
  \* About to read.
  /\ pc[o] = "pop"
  \* The word as it is now.
  /\ head' = [head EXCEPT ![o] = free]
  \* An empty stack: a new slot. Else: read below the top.
  /\ pc' = [pc EXCEPT ![o] = IF free.top = 0 THEN "fresh" ELSE "below"]
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, claimVars, ip, open, nbegun, tx, next, path, held, closeVars>>

\* take: read the slot below the top it read; 0 when its chunk is not
\* there.
Below(o) ==
  \* About to read below.
  /\ pc[o] = "below"
  \* below[top], as the chunk holds it now.
  /\ next' = [next EXCEPT ![o] =
               IF ChunkOf(head[o].top) \in chunks THEN below[head[o].top] ELSE 0]
  \* Then the CAS.
  /\ pc' = [pc EXCEPT ![o] = "popcas"]
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, claimVars, ip, open, nbegun, tx, head, path, held, closeVars>>

\* take: the CAS from the word read to "below on top". It holds the slot
\* when it lands; when it fails it carries the word it found.
PopCas(o) ==
  \* About to CAS.
  /\ pc[o] = "popcas"
  \* The word is still the one read: the slot is this transaction's.
  /\ IF free = head[o]
     \* The stack loses its top.
     THEN /\ free' = [tag |-> NextTag(head[o].tag), top |-> next[o]]
          \* The transaction holds the slot.
          /\ slotOf' = [slotOf EXCEPT ![tx[o]] = head[o].top]
          \* Then its chunk.
          /\ pc' = [pc EXCEPT ![o] = "init"]
          \* The word carried stays.
          /\ UNCHANGED head
     \* The word changed: go again with the word found.
     ELSE /\ head' = [head EXCEPT ![o] = free]
          \* Empty now: a new slot; else read below the new top.
          /\ pc' = [pc EXCEPT ![o] = IF free.top = 0 THEN "fresh" ELSE "below"]
          \* Nothing taken.
          /\ UNCHANGED <<free, slotOf>>
  \* Nothing else changes.
  /\ UNCHANGED <<below, taken, chunks, cell, closed, cstate, given, dropBy, ip, open, nbegun,
                 tx, next, path, held, closeVars>>

\* take: a new slot, counted up; none once the table is full (reserve
\* returns None and the claim is not listed).
Fresh(o) ==
  \* About to count up.
  /\ pc[o] = "fresh"
  \* One more handed out.
  /\ taken' = taken + 1
  \* Inside the table: that slot is this transaction's.
  /\ IF taken + 1 <= Cap
     \* It holds the new slot.
     THEN /\ slotOf' = [slotOf EXCEPT ![tx[o]] = taken + 1]
          \* Then its chunk.
          /\ pc' = [pc EXCEPT ![o] = "init"]
     \* Full: not listed.
     ELSE /\ pc' = [pc EXCEPT ![o] = "finish"]
          \* It holds nothing.
          /\ UNCHANGED slotOf
  \* Nothing else changes.
  /\ UNCHANGED <<free, below, chunks, cell, closed, cstate, given, dropBy, ip, open, nbegun,
                 tx, head, next, path, held, closeVars>>

\* fill: the slot's chunk is made if it is not there yet (get_or_init_racy:
\* racing threads each build one, and one compare-and-swap publishes it).
Init_(o) ==
  \* About to find the chunk.
  /\ pc[o] = "init"
  \* It is there from now on.
  /\ chunks' = chunks \cup {ChunkOf(SlotOfTx(o))}
  \* Then the store.
  /\ pc' = [pc EXCEPT ![o] = "fill"]
  \* Nothing else changes.
  /\ UNCHANGED <<free, below, taken, cell, claimVars, ip, open, nbegun, tx, head, next, path,
                 held, closeVars>>

\* fill: the claim's reference goes into the slot.
Fill(o) ==
  \* About to store.
  /\ pc[o] = "fill"
  \* The slot holds the claim.
  /\ cell' = [cell EXCEPT ![SlotOfTx(o)] = tx[o]]
  \* The transaction is listed and open.
  /\ pc' = [pc EXCEPT ![o] = "finish"]
  \* Nothing else changes.
  /\ UNCHANGED <<free, below, taken, chunks, claimVars, ip, open, nbegun, tx, head, next, path,
                 held, closeVars>>

\* begin_commit: claim the commit, OPEN -> COMMITTING. If close aborted the
\* transaction first, the commit fails with Closed. Then remove.
Commit(o) ==
  \* About to claim the commit.
  /\ pc[o] = "commit"
  \* Still open: committing. Else close ended it; nothing changes.
  /\ cstate' = [cstate EXCEPT ![tx[o]] = IF @ = "open" THEN "committing" ELSE @]
  \* Then empty and give back the slot, if it has one.
  /\ pc' = [pc EXCEPT ![o] = IF SlotOfTx(o) # 0 THEN FirstRemoveStep ELSE "finish"]
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, closed, slotOf, given, dropBy, ip, open, nbegun, tx, head, next,
                 path, held, closeVars>>

\* remove: swap the reference out of the slot; whoever gets it drops it.
\* TakeByLoad reads it here and empties the slot in a second step.
Swap(o) ==
  \* About to swap.
  /\ pc[o] = "swap"
  \* s is the slot.
  /\ LET s == SlotOfTx(o) IN
       \* The bug: only read the slot now.
       IF Mutant = "TakeByLoad"
       \* What it read.
       THEN /\ held' = [held EXCEPT ![o] = cell[s]]
            \* Then the store.
            /\ pc' = [pc EXCEPT ![o] = "store"]
            \* The slot is not emptied yet.
            /\ UNCHANGED <<cell, dropBy>>
       \* The real swap: the slot is empty, and what it held is dropped.
       ELSE /\ cell' = [cell EXCEPT ![s] = None]
            \* By this owner, if anything was there.
            /\ dropBy' = IF cell[s] # None
                         \* (it was something: dropped by o)
                         THEN [dropBy EXCEPT ![cell[s]] = @ \cup {o}]
                         \* (it was empty: nothing dropped)
                         ELSE dropBy
            \* Then the push (or, GiveBackFirst, the end).
            /\ pc' = [pc EXCEPT ![o] = AfterSwap(o)]
            \* Nothing read separately.
            /\ UNCHANGED held
  \* Nothing else changes.
  /\ UNCHANGED <<free, below, taken, chunks, closed, cstate, slotOf, given, ip, open, nbegun,
                 tx, head, next, path, closeVars>>

\* TakeByLoad, second step: empty the slot and drop what was read.
Store(o) ==
  \* About to store.
  /\ pc[o] = "store"
  \* The slot is emptied.
  /\ cell' = [cell EXCEPT ![SlotOfTx(o)] = None]
  \* What was read is dropped by this owner.
  /\ dropBy' = IF held[o] # None
               \* (it read something: dropped by o)
               THEN [dropBy EXCEPT ![held[o]] = @ \cup {o}]
               \* (it read nothing: nothing dropped)
               ELSE dropBy
  \* Then the push.
  /\ pc' = [pc EXCEPT ![o] = AfterSwap(o)]
  \* Nothing else changes.
  /\ UNCHANGED <<free, below, taken, chunks, closed, cstate, slotOf, given, ip, open, nbegun,
                 tx, head, next, path, held, closeVars>>

\* remove: read the free word.
PushLoad(o) ==
  \* About to read.
  /\ pc[o] = "pushload"
  \* The word as it is now.
  /\ head' = [head EXCEPT ![o] = free]
  \* Then name the slot below.
  /\ pc' = [pc EXCEPT ![o] = "pushbelow"]
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, claimVars, ip, open, nbegun, tx, next, path, held, closeVars>>

\* remove: below[slot] := the top read. Only the holder writes it.
PushBelow(o) ==
  \* About to write below.
  /\ pc[o] = "pushbelow"
  \* The slot names the top it read.
  /\ below' = [below EXCEPT ![SlotOfTx(o)] = head[o].top]
  \* Then the CAS.
  /\ pc' = [pc EXCEPT ![o] = "pushcas"]
  \* Nothing else changes.
  /\ UNCHANGED <<free, taken, chunks, cell, claimVars, ip, open, nbegun, tx, head, next, path,
                 held, closeVars>>

\* remove: the CAS from the word read to "this slot on top". When it fails
\* it writes below again from the word found.
PushCas(o) ==
  \* About to CAS.
  /\ pc[o] = "pushcas"
  \* The word is still the one read: the slot is on the stack.
  /\ IF free = head[o]
     \* The slot is the new top.
     THEN /\ free' = [tag |-> NextTag(head[o].tag), top |-> SlotOfTx(o)]
          \* The transaction no longer holds it.
          /\ given' = [given EXCEPT ![tx[o]] = TRUE]
          \* Then the end (or, GiveBackFirst, the swap).
          /\ pc' = [pc EXCEPT ![o] = AfterPush(o)]
          \* The word carried stays.
          /\ UNCHANGED head
     \* The word changed: go again from the word found.
     ELSE /\ head' = [head EXCEPT ![o] = free]
          \* Write below again.
          /\ pc' = [pc EXCEPT ![o] = "pushbelow"]
          \* Nothing given back.
          /\ UNCHANGED <<free, given>>
  \* Nothing else changes.
  /\ UNCHANGED <<below, taken, chunks, cell, closed, cstate, slotOf, dropBy, ip, open, nbegun,
                 tx, next, path, held, closeVars>>

\* owner_abort after a rollback's remove: OPEN -> ABORTED, unless close
\* got there first.
Abort(o) ==
  \* About to end the claim.
  /\ pc[o] = "abort"
  \* Still open: aborted. Else it stays as close left it.
  /\ cstate' = [cstate EXCEPT ![tx[o]] = IF @ = "open" THEN "aborted" ELSE @]
  \* Then the end.
  /\ pc' = [pc EXCEPT ![o] = "finish"]
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, closed, slotOf, given, dropBy, ip, open, nbegun, tx, head, next,
                 path, held, closeVars>>

\* The script entry is done: a begun transaction joins the open ones, an
\* ended one leaves them.
Finish(o) ==
  \* At the end of an entry.
  /\ pc[o] = "finish"
  \* A begin adds the transaction; an end removes the oldest.
  /\ open' = [open EXCEPT ![o] =
               IF Script[o][ip[o]] = "begin" THEN Append(@, tx[o]) ELSE Tail(@)]
  \* The next entry.
  /\ ip' = [ip EXCEPT ![o] = ip[o] + 1]
  \* Back between entries.
  /\ pc' = [pc EXCEPT ![o] = "idle"]
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, claimVars, nbegun, tx, head, next, path, held, closeVars>>

\* ---------------------------------------------------------------------------
\* Close.
\* ---------------------------------------------------------------------------

\* close begins: the database is closing (close_state = CLOSING).
Close ==
  \* Close runs in this configuration and has not begun.
  /\ Closes /\ cpc = "idle"
  \* From now on Claim::track lists nothing.
  /\ closed' = TRUE
  \* Then the drain.
  /\ cpc' = "bound"
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, cstate, slotOf, given, dropBy, ownerVars, bound, ci, chand, live,
                 panicked>>

\* drain: read how many slots were handed out; it looks at that many, at
\* most the table's capacity (NoClamp: all of them).
Bound ==
  \* About to read `taken`.
  /\ cpc = "bound"
  \* The slots it looks at.
  /\ bound' = IF Mutant = "NoClamp" THEN taken ELSE Min(taken, Cap)
  \* Ghost: every claim listed now.
  /\ live' = {cell[s] : s \in Slots} \ {None}
  \* From slot 1.
  /\ cpc' = "visit"
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, claimVars, ownerVars, ci, chand, panicked>>

\* drain: look at slot ci. Past the bound it is done; past the table it
\* panics (index out of bounds); a slot whose chunk is not there is
\* skipped; else the slot's reference is swapped out (TakeByLoad: read).
Visit ==
  \* At a slot.
  /\ cpc = "visit"
  \* Past the bound: done.
  /\ IF ci > bound
     \* The drain returns.
     THEN /\ cpc' = "done"
          \* Nothing else changes.
          /\ UNCHANGED <<cell, dropBy, ci, chand, panicked>>
     \* Past the table: chunks[place(slot)] is out of range.
     ELSE IF ci > Cap
     \* It panics and stops.
     THEN /\ panicked' = TRUE
          \* No further step.
          /\ cpc' = "done"
          \* Nothing else changes.
          /\ UNCHANGED <<cell, dropBy, ci, chand>>
     \* The chunk is not there: nothing to take, next slot.
     ELSE IF ChunkOf(ci) \notin chunks
     \* Next slot.
     THEN /\ ci' = ci + 1
          \* Nothing else changes.
          /\ UNCHANGED <<cell, dropBy, chand, panicked, cpc>>
     \* The bug StopAtEmpty: an empty slot ends the drain.
     ELSE IF Mutant = "StopAtEmpty" /\ cell[ci] = None
     \* Done early.
     THEN /\ cpc' = "done"
          \* Nothing else changes.
          /\ UNCHANGED <<cell, dropBy, ci, chand, panicked>>
     \* The bug TakeByLoad: read only.
     ELSE IF Mutant = "TakeByLoad"
     \* What it read.
     THEN /\ chand' = cell[ci]
          \* Then the store.
          /\ cpc' = "store"
          \* Nothing else changes.
          /\ UNCHANGED <<cell, dropBy, ci, panicked>>
     \* The swap: the slot is emptied; close owns what it held.
     ELSE /\ cell' = [cell EXCEPT ![ci] = None]
          \* What it held is close's to drop.
          /\ dropBy' = IF cell[ci] # None
                       \* (a claim: close drops it)
                       THEN [dropBy EXCEPT ![cell[ci]] = @ \cup {0}]
                       \* (empty: nothing dropped)
                       ELSE dropBy
          \* Held for the abort.
          /\ chand' = cell[ci]
          \* A claim: abort it. Nothing: next slot.
          /\ IF cell[ci] # None
             \* Abort it next.
             THEN /\ cpc' = "abort"
                  \* Same slot until the abort is done.
                  /\ UNCHANGED ci
             \* Next slot.
             ELSE /\ ci' = ci + 1
                  \* Still visiting.
                  /\ UNCHANGED cpc
          \* No panic.
          /\ UNCHANGED panicked
  \* Nothing else changes.
  /\ UNCHANGED <<free, below, taken, chunks, closed, cstate, slotOf, given, ownerVars, bound,
                 live>>

\* TakeByLoad, close's second step: empty the slot, drop what was read.
CloseStore ==
  \* About to store.
  /\ cpc = "store"
  \* The slot is emptied.
  /\ cell' = [cell EXCEPT ![ci] = None]
  \* What was read is close's to drop.
  /\ dropBy' = IF chand # None THEN [dropBy EXCEPT ![chand] = @ \cup {0}] ELSE dropBy
  \* A claim: abort it. Nothing: next slot.
  /\ IF chand # None
     \* Abort next.
     THEN /\ cpc' = "abort"
          \* Same slot.
          /\ UNCHANGED ci
     \* Next slot.
     ELSE /\ ci' = ci + 1
          \* Still visiting.
          /\ cpc' = "visit"
  \* Nothing else changes.
  /\ UNCHANGED <<free, below, taken, chunks, closed, cstate, slotOf, given, ownerVars, bound,
                 chand, live, panicked>>

\* abort_for_close: OPEN -> ABORTED, unless the owner got there first.
CloseAbort ==
  \* About to abort the claim taken.
  /\ cpc = "abort"
  \* Still open: aborted. Else the owner ended it.
  /\ cstate' = [cstate EXCEPT ![chand] = IF @ = "open" THEN "aborted" ELSE @]
  \* Next slot.
  /\ ci' = ci + 1
  \* Visiting again.
  /\ cpc' = "visit"
  \* Nothing else changes.
  /\ UNCHANGED <<tableVars, closed, slotOf, given, dropBy, ownerVars, bound, chand, live,
                 panicked>>

\* ---------------------------------------------------------------------------
\* The whole system.
\* ---------------------------------------------------------------------------

\* Every owner has run its whole script, and close is done or never runs.
AllDone ==
  \* Every owner is between entries with none left.
  /\ \A o \in Owners : pc[o] = "idle" /\ ip[o] > Len(Script[o])
  \* Close returned, or never ran.
  /\ cpc = "done" \/ ~Closes

\* Nothing is left to do: the run stays here (so a state where nothing can
\* move and not everybody is done shows up as a deadlock).
Rest ==
  \* Everybody is done.
  /\ AllDone
  \* Nothing changes.
  /\ UNCHANGED vars

\* One owner's step.
OwnerStep(o) ==
  \* Any of them.
  \/ Start(o) \/ Check(o) \/ Pop(o) \/ Below(o) \/ PopCas(o) \/ Fresh(o) \/ Init_(o)
  \* Or the end's.
  \/ Fill(o) \/ Commit(o) \/ Swap(o) \/ Store(o) \/ PushLoad(o) \/ PushBelow(o) \/ PushCas(o)
  \* Or the last ones.
  \/ Abort(o) \/ Finish(o)

\* The closer's step.
CloseStep == Close \/ Bound \/ Visit \/ CloseStore \/ CloseAbort

\* Every step anyone can take.
Next ==
  \* An owner's.
  \/ \E o \in Owners : OwnerStep(o)
  \* The closer's.
  \/ CloseStep
  \* Or rest at the end.
  \/ Rest

\* The behaviours: start in Init, take Next steps.
Spec == Init /\ [][Next]_vars

\* ===========================================================================
\* THE RULES
\* ===========================================================================

\* Transaction t holds its slot: it took one and has not given it back.
Holding(t) == slotOf[t] # 0 /\ ~given[t]
\* The slots some transaction holds.
HeldSlots == {slotOf[t] : t \in {u \in Tx : Holding(u)}}

\* No slot is held by two transactions at once. Rules out: two transactions
\* list their claims in one slot, and one's end drops the other's.
OneHolder ==
  \* Two different holders hold different slots.
  \A t, u \in Tx : t # u /\ Holding(t) /\ Holding(u) => slotOf[t] # slotOf[u]

\* The k-th slot of the free stack from the top (k = 1 is the top), or 0
\* past its end.
RECURSIVE Nth(_, _)
\* Follow below k - 1 times.
Nth(s, k) == IF k = 1 \/ s = 0 THEN s ELSE Nth(below[s], k - 1)
\* The free stack as a list, Cap + 1 long so a loop shows as no 0 in it.
StackList == [k \in 1..(Cap + 1) |-> Nth(free.top, k)]
\* How many slots the stack holds before its end.
Depth == CHOOSE d \in 0..Cap : StackList[d + 1] = 0 /\ \A e \in 1..d : StackList[e] # 0
\* The slots on the stack.
OnStack == {StackList[k] : k \in 1..Depth}

\* The free stack ends, holds each slot once, holds no slot a transaction
\* holds, and with the held ones is exactly every slot ever handed out.
\* Rules out (the ABA race): a slot on the stack that a transaction holds,
\* so the next taker gets it too.
FreeStackSound ==
  \* It ends within Cap slots (no loop).
  /\ \E k \in 1..(Cap + 1) : StackList[k] = 0
  \* No slot twice.
  /\ \A i, j \in 1..Depth : StackList[i] = StackList[j] => i = j
  \* No held slot on it.
  /\ OnStack \cap HeldSlots = {}
  \* None lost: every slot handed out is held or free.
  /\ OnStack \cup HeldSlots = 1..Min(taken, Cap)

\* A slot holds only the claim of the transaction holding it. Rules out: a
\* slot given back while it still holds its claim, which the next taker
\* then overwrites (that claim is never dropped) or its first owner swaps
\* out (the wrong claim is dropped).
CellBelongsToHolder ==
  \* Every claim in a slot is its holder's.
  \A s \in Slots : cell[s] # None => Holding(cell[s]) /\ slotOf[cell[s]] = s

\* A slot's reference is dropped at most once, by the claim's owner or by
\* close. Rules out: owner and close both taking it.
DroppedOnce ==
  \* For every claim.
  \A t \in Tx : Cardinality(dropBy[t]) <= 1 /\ dropBy[t] \subseteq {t[1], 0}

\* When close is done, every claim listed when its drain began has been
\* taken out of its slot: by close, which aborted it, or by its owner, who
\* ended it. Rules out: a drain that misses a listed claim, whose callbacks
\* then wait on a caller that will not return.
LiveTakenByClose ==
  \* Close done: every claim it found listed is out.
  cpc = "done" => \A t \in live : dropBy[t] # {}

\* The drain never looks past the table. Rules out: a drain that reads
\* `taken` slots when more were counted than the table holds.
DrainInBounds == ~panicked

\* Once everybody is done: every claim begun is ended, no slot holds a
\* reference, every reference was dropped exactly once, and every slot is
\* back on the stack. Rules out: a claim lost (never ended, or its slot
\* never given back).
AtRest ==
  \* Everybody done ...
  AllDone =>
    \* ... every begun claim ended,
    /\ \A t \in Tx : cstate[t] \in {"committing", "aborted"}
    \* no slot holds a claim,
    /\ \A s \in Slots : cell[s] = None
    \* every listed reference dropped once and its slot given back,
    /\ \A t \in Tx : slotOf[t] # 0 => Cardinality(dropBy[t]) = 1 /\ given[t]
    \* and nothing is held.
    /\ HeldSlots = {}

\* ===========================================================================
\* WITNESSES: each must break in a GREEN run, which shows that run reaches
\* the race the rules above are about.
\* ===========================================================================

\* "No taker is ever about to CAS a word whose top is the one it read while
\* the stack under that top changed": the ABA race. It breaks in
\* MC_ClaimSlots_Green_ReachesABA, where the tag then fails the CAS.
NeverABA ==
  \* No owner at its CAS with the same top and a changed stack below it.
  ~\E o \in Owners : /\ pc[o] = "popcas"
                    \* (the top it read is the top now)
                    /\ free.top = head[o].top
                    \* (but the word changed)
                    /\ free # head[o]
                    \* (and what lies below the top is not what it read)
                    /\ next[o] # below[free.top]

\* "Close never aborts a transaction before its owner claims the commit":
\* breaks in MC_ClaimSlots_Green_ReachesCloseFirst, where that commit then
\* fails with Closed.
NeverCloseFirst ==
  \* No owner about to claim a commit close already aborted.
  ~\E o \in Owners : pc[o] = "commit" /\ cstate[tx[o]] = "aborted"

\* "Close never looks at a handed-out slot whose chunk is not made yet":
\* breaks in MC_ClaimSlots_Green_ReachesLateChunk, where close skips the
\* slot and its owner then lists the claim and ends it itself.
NeverLateChunk ==
  \* Close is at a slot inside the table ...
  ~ /\ cpc = "visit" /\ ci <= Min(bound, Cap)
    \* ... whose chunk is not there ...
    /\ ChunkOf(ci) \notin chunks
    \* ... and which a transaction holds.
    /\ \E t \in Tx : slotOf[t] = ci /\ Holding(t)

====
