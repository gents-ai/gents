---- MODULE ClockCache ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* regolith keeps recently read table blocks in a block cache, so a second
\* read of the same block costs no disk read (src/engine/block_cache.rs).
\* The cache has a byte budget. When a new block does not fit, a "hand"
\* walks round the cached entries in a circle (CLOCK) and evicts one:
\*   - an entry read since the hand last passed has its "referenced" bit
\*     set; the hand clears the bit and passes it by, a second chance;
\*   - an entry a reader is holding right now is "pinned": the hand never
\*     evicts it. A reader's pin is a reference to the block, and the hand
\*     evicts only by turning the cache's own lone reference into none in
\*     one compare-and-swap, which fails while any other reference exists.
\* Nothing here takes a lock: every step is one compare-and-swap.
\*
\* Blocks also arrive another way. A thread that must never wait reads
\* through its own I/O queue (CacheOnly reads, src/engine/io): on a miss it
\* records the block on its queue and comes back later. When the read
\* finishes, the block "lands": it goes into the cache, and the queue keeps
\* a reference to it in its landing table, so the read that runs again is
\* sure to find it even if the cache had to evict it meanwhile.
\*
\* Tiny example. The budget holds two blocks. Reader R reads block 1 and
\* holds it. An insert of block 3 needs room: the hand reaches block 1, sees
\* R's pin, and passes; it reaches block 2, nobody holds it, and evicts it.
\* R's block 1 is never freed under R.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - the hand evicts a block a reader holds, and the block's memory is
\*     freed under the reader (NoFreedPin);
\*   - the cache holds more bytes than its budget (ByteBound);
\*   - the bytes the cache counts drift from what it holds (UsedExact);
\*   - a block that landed for a waiting queue is freed before that queue's
\*     read runs again, so the read finds nothing (LandedReadable);
\*   - a reference is lost or counted twice (RefsExact), which is what all
\*     of the above stand on.
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   cached    the blocks the cache holds: an entry published in a ring
\*             slot and filed in the shard's map (block_cache/ring.rs)
\*   refs      each block's strong count: the cache's own reference, one
\*             per reader holding it, one per landing that names it
\*             (Arc<Block>, the pin; Weak::upgrade takes one)
\*   live      the blocks whose memory exists: read and not yet freed
\*   refbit    the REF flag of the block's slot word
\*   used      the bytes charged against the budget (total_used)
\*   pins      what each reader holds (the Arc<Block> a get returned)
\*   wants     what each queue's CacheOnly read missed and waits for
\*   landed    each queue's landing table (QueueShared::landed)
\*   reran     the waits each queue's read has been run again for (ghost)
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Shards. Every shard runs this same protocol on its own slots and
\*     its own share of the budget; the cache-wide total is reserved first,
\*     which is the same bounded compare-and-swap at one more level.
\*   - The map and the slot generations. They decide which slot an entry
\*     lives in, never whether a pin is honoured or bytes are counted.
\*   - Memory ordering and the CAS loops: tests/loom_block_cache.rs checks
\*     those. Here each CAS is one atomic step.
\*   - Explicit removals (a re-insert of a key, evict_file, clear). They
\*     drop the cache's reference whatever readers hold, and a reader keeps
\*     its own, which this model's reference counting already covers.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one
\* invariant it names):
\*   MC_ClockCache_Green               the real protocol.
\*   MC_ClockCache_Red_IgnorePins      the hand evicts whatever it lands
\*                                     on and frees it: NoFreedPin breaks.
\*   MC_ClockCache_Red_Unbounded       an insert never checks the budget:
\*                                     ByteBound breaks.
\*   MC_ClockCache_Red_LandingUnheld   a landing keeps no reference of its
\*                                     own, so the hand frees what landed:
\*                                     LandedReadable breaks.
\*
\* Lean, proofs/lean/Regolith/ClockCache.lean, proves the same rules for
\* every number of blocks, readers and queues.

\* Numbers and finite sets.
EXTENDS Naturals, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The blocks that may be read.
  Blocks,
  \* The reader threads (Blocking reads that pin what they get).
  Readers,
  \* The I/O queues of threads that never wait (CacheOnly reads).
  Queues,
  \* [Blocks -> Nat]: what each block costs the budget.
  Size,
  \* The byte budget.
  Cap,
  \* "Green" for the real code, or the name of one planted bug.
  Mode

\* The bug names this model knows.
ASSUME Mode \in {"Green", "IgnorePins", "Unbounded", "LandingUnheld"}
\* Sizes and the budget are numbers.
ASSUME Size \in [Blocks -> Nat] /\ Cap \in Nat

\* The state that changes from step to step.
VARIABLES
  \* SUBSET Blocks: what the cache holds.
  cached,
  \* [Blocks -> Nat]: each block's strong count.
  refs,
  \* SUBSET Blocks: blocks whose memory exists.
  live,
  \* [Blocks -> BOOLEAN]: read since the hand last passed.
  refbit,
  \* Bytes charged against the budget.
  used,
  \* [Readers -> SUBSET Blocks]: what each reader holds.
  pins,
  \* [Queues -> SUBSET Blocks]: what each queue waits for.
  wants,
  \* [Queues -> SUBSET Blocks]: what landed for each queue.
  landed,
  \* [Queues -> SUBSET Blocks]: the waits already run again (a ghost that
  \* keeps each wait to one round, so the model is finite).
  reran

\* Every variable, so a step that changes none of them is a stutter.
vars == <<cached, refs, live, refbit, used, pins, wants, landed, reran>>

\* The sum of the sizes of a set of blocks, one block at a time.
RECURSIVE SumSize(_)
\* Its definition, taking one block out of the set at a time.
SumSize(S) ==
  \* Nothing costs nothing ...
  IF S = {} THEN 0
  \* ... otherwise take any one block's size and add the rest's.
  ELSE LET b == CHOOSE x \in S : TRUE IN Size[b] + SumSize(S \ {b})

\* Every block costs one unit of the budget: what the configurations use.
\* (A .cfg file cannot spell a function, so it names this one.)
UnitSizes == [b \in Blocks |-> 1]

\* How many readers hold block b.
Holders(b) == Cardinality({r \in Readers : b \in pins[r]})

\* How many queues' landing tables name block b.
Landings(b) == Cardinality({q \in Queues : b \in landed[q]})

\* The start: an empty cache, nothing read, nobody holding or waiting.
Init ==
  \* The cache holds nothing ...
  /\ cached = {}
  \* ... no block has a reference ...
  /\ refs = [b \in Blocks |-> 0]
  \* ... none is in memory ...
  /\ live = {}
  \* ... no reference bit is set ...
  /\ refbit = [b \in Blocks |-> FALSE]
  \* ... and no byte is charged.
  /\ used = 0
  \* No reader holds anything.
  /\ pins = [r \in Readers |-> {}]
  \* No queue waits for anything ...
  /\ wants = [q \in Queues |-> {}]
  \* ... has anything landed ...
  /\ landed = [q \in Queues |-> {}]
  \* ... or has run a read again.
  /\ reran = [q \in Queues |-> {}]

\* Whether a block of size s fits the budget now (bounded_add's check, done
\* in the same compare-and-swap as the add). The Unbounded bug skips it.
Fits(s) == Mode = "Unbounded" \/ used + s <= Cap

\* A Blocking read of block b misses: the reader reads it from the table,
\* keeps it (its pin), and offers it to the cache. The cache takes it only
\* if it fits; a refused block is the reader's alone.
Miss(r, b) ==
  \* A block not in memory, which this reader does not hold.
  /\ b \notin live
  \* The reader now holds a fresh copy: memory exists, one reference.
  /\ live' = live \cup {b}
  \* The reader holds (pins) the block.
  /\ pins' = [pins EXCEPT ![r] = @ \cup {b}]
  \* The cache's insert: reserve the bytes, publish the entry with its
  \* referenced bit clear, and take the cache's own reference.
  /\ IF Fits(Size[b])
       \* Admitted.
       THEN
            \* The entry is cached ...
            /\ cached' = cached \cup {b}
            \* ... its bytes are charged ...
            /\ used' = used + Size[b]
            \* ... two references: the reader's and the cache's ...
            /\ refs' = [refs EXCEPT ![b] = 2]
            \* ... and it starts with its bit clear.
            /\ refbit' = [refbit EXCEPT ![b] = FALSE]
       \* Refused.
       ELSE
            \* Only the reader's reference exists ...
            /\ refs' = [refs EXCEPT ![b] = 1]
            \* ... and the cache is as it was.
            /\ UNCHANGED <<cached, used, refbit>>
  \* Queues are not touched.
  /\ UNCHANGED <<wants, landed, reran>>

\* A reader's get hits block b: it upgrades the map's weak handle, which is
\* its pin, and sets the referenced bit.
Hit(r, b) ==
  \* A cached block ...
  /\ b \in cached
  \* ... this reader does not already hold.
  /\ b \notin pins[r]
  \* One more reference: the reader's ...
  /\ refs' = [refs EXCEPT ![b] = @ + 1]
  \* ... and the reader now holds it.
  /\ pins' = [pins EXCEPT ![r] = @ \cup {b}]
  \* Read since the hand last passed.
  /\ refbit' = [refbit EXCEPT ![b] = TRUE]
  \* Nothing else changes.
  /\ UNCHANGED <<cached, live, used, wants, landed, reran>>

\* A reader drops a block it held. The last reference frees the memory.
Drop(r, b) ==
  \* Only a block this reader holds.
  /\ b \in pins[r]
  \* It holds it no more ...
  /\ pins' = [pins EXCEPT ![r] = @ \ {b}]
  \* ... one reference fewer.
  /\ refs' = [refs EXCEPT ![b] = @ - 1]
  \* If that was the last reference, the block is gone.
  /\ live' = IF refs[b] = 1 THEN live \ {b} ELSE live
  \* Nothing else changes.
  /\ UNCHANGED <<cached, refbit, used, wants, landed, reran>>

\* One step of the hand landing on cached block b. On a pass that honours
\* reference bits (forced = FALSE) a set bit is cleared and the entry kept.
\* Otherwise the hand evicts the entry only if the cache's reference is the
\* only one (the compare-and-swap from one to zero); a pinned entry is
\* passed over. The IgnorePins bug evicts and frees it regardless.
Hand(b, forced) ==
  \* The hand only lands on cached entries.
  /\ b \in cached
  \* Is the entry's bit set, and the hand not forced?
  /\ IF ~forced /\ refbit[b]
       \* Second chance.
       THEN
            \* The bit is cleared,
            /\ refbit' = [refbit EXCEPT ![b] = FALSE]
            \* and the entry stays.
            /\ UNCHANGED <<cached, refs, live, used>>
       \* Otherwise: is the bug IgnorePins planted?
       ELSE IF Mode = "IgnorePins"
         \* The bug: drop the entry and free the block as if the cache held the only reference.
         THEN
              \* The entry goes ...
              /\ cached' = cached \ {b}
              \* ... its bytes come back ...
              /\ used' = used - Size[b]
              \* ... the cache's reference goes ...
              /\ refs' = [refs EXCEPT ![b] = @ - 1]
              \* ... and the memory is freed, whoever still holds it.
              /\ live' = live \ {b}
              \* The bit does not matter any more.
              /\ UNCHANGED refbit
         \* Otherwise, the design: is the cache's reference the only one?
         ELSE IF refs[b] = 1
           \* Nobody else holds it: the compare-and-swap from one to zero wins.
           THEN
                \* The entry goes ...
                /\ cached' = cached \ {b}
                \* ... its bytes come back ...
                /\ used' = used - Size[b]
                \* ... no reference is left ...
                /\ refs' = [refs EXCEPT ![b] = 0]
                \* ... so the block is freed.
                /\ live' = live \ {b}
                \* The bit does not matter any more.
                /\ UNCHANGED refbit
           \* Pinned: the CAS fails and the hand moves on.
           ELSE
                \* Nothing changes.
                UNCHANGED <<cached, refs, live, used, refbit>>
  \* Readers and queues are not touched by the hand.
  /\ UNCHANGED <<pins, wants, landed, reran>>

\* A CacheOnly read on queue q misses block b: it waits for b on q.
Want(q, b) ==
  \* Not cached (a hit would not wait) ...
  /\ b \notin cached
  \* ... not already waited for ...
  /\ b \notin wants[q]
  \* ... and not waited for before (one round per wait keeps it finite).
  /\ b \notin reran[q]
  \* The queue now waits for b.
  /\ wants' = [wants EXCEPT ![q] = @ \cup {b}]
  \* Nothing else changes.
  /\ UNCHANGED <<cached, refs, live, refbit, used, pins, landed, reran>>

\* The unit that reads block b for queue q finishes (Unit::run, then the
\* owner's poll): the block is read, the queue's landing table keeps it with
\* a reference of its own, and the cache is offered it as on a miss. The
\* LandingUnheld bug keeps it in the landing table without a reference.
Land(q, b) ==
  \* The queue waits for b ...
  /\ b \in wants[q]
  \* ... nothing landed for it yet ...
  /\ b \notin landed[q]
  \* ... and b is not in memory: the unit reads it fresh.
  /\ b \notin live
  \* The block now exists ...
  /\ live' = live \cup {b}
  \* ... and the queue's landing names it.
  /\ landed' = [landed EXCEPT ![q] = @ \cup {b}]
  \* The landing's own reference, unless the bug drops it.
  /\ LET held == IF Mode = "LandingUnheld" THEN 0 ELSE 1 IN
       \* Do the block's bytes fit in the budget?
       IF Fits(Size[b])
         \* Admitted.
         THEN
              \* The entry is cached ...
              /\ cached' = cached \cup {b}
              \* ... its bytes are charged ...
              /\ used' = used + Size[b]
              \* ... with the landing's reference and the cache's ...
              /\ refs' = [refs EXCEPT ![b] = held + 1]
              \* ... and its bit clear.
              /\ refbit' = [refbit EXCEPT ![b] = FALSE]
         \* Refused.
         ELSE
              \* Only the landing holds it ...
              /\ refs' = [refs EXCEPT ![b] = held]
              \* ... and the cache is as it was.
              /\ UNCHANGED <<cached, used, refbit>>
  \* Readers are not touched.
  /\ UNCHANGED <<pins, wants, reran>>

\* The queue's read of block b runs again and finds it: from what landed for
\* the queue, or from the cache. It then lets the landing go.
Rerun(q, b) ==
  \* It waits for b ...
  /\ b \in wants[q]
  \* ... and b is where the read looks: landed, or cached.
  /\ b \in landed[q] \/ b \in cached
  \* The wait is over ...
  /\ wants' = [wants EXCEPT ![q] = @ \ {b}]
  \* ... and the model remembers it ran.
  /\ reran' = [reran EXCEPT ![q] = @ \cup {b}]
  \* The owner forgets the landing, giving back its reference (none, under
  \* the bug); the last reference frees the block.
  /\ landed' = [landed EXCEPT ![q] = @ \ {b}]
  \* How many references the landing gives back: one if the queue had a landing of b (and the bug is
  \* not planted), else none.
  /\ LET gives == IF b \in landed[q] /\ Mode # "LandingUnheld" THEN 1 ELSE 0 IN
       \* One reference fewer when the landing held one ...
       /\ refs' = [refs EXCEPT ![b] = @ - gives]
       \* ... and if it was the last, the block is freed.
       /\ live' = IF gives = 1 /\ refs[b] = 1 THEN live \ {b} ELSE live
  \* Nothing else changes.
  /\ UNCHANGED <<cached, refbit, used, pins>>

\* Every step the system can take.
Next ==
  \* A reader misses, hits or drops some block ...
  \/ \E r \in Readers, b \in Blocks : Miss(r, b) \/ Hit(r, b) \/ Drop(r, b)
  \* ... or the hand lands on some entry, honouring bits or not ...
  \/ \E b \in Blocks, forced \in BOOLEAN : Hand(b, forced)
  \* ... or a queue waits, a block lands for it, or its read runs again.
  \/ \E q \in Queues, b \in Blocks : Want(q, b) \/ Land(q, b) \/ Rerun(q, b)

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* Only blocks are cached ...
  /\ cached \subseteq Blocks
  \* ... or in memory.
  /\ live \subseteq Blocks
  \* Every block has a count ...
  /\ refs \in [Blocks -> Nat]
  \* ... the charge is a number ...
  /\ used \in Nat
  \* ... readers hold blocks ...
  /\ pins \in [Readers -> SUBSET Blocks]
  \* ... and landings name blocks.
  /\ landed \in [Queues -> SUBSET Blocks]

\* THE HEADLINE. A block a reader holds is never freed: the hand never
\* evicts a pinned block out from under it. Rules out: R gets block 1, the
\* hand needs room, takes block 1 and frees it while R reads its bytes.
\* Lean: evict_only_unpinned, reachable_safe.
NoFreedPin ==
  \* Everything every reader holds is in memory.
  \A r \in Readers : pins[r] \subseteq live

\* The cache never holds more bytes than its budget. Rules out: two inserts
\* each see room for one more block and both go in. Lean: reserve_bounded,
\* reachable_safe.
ByteBound ==
  \* The charge is within the budget.
  used <= Cap

\* The bytes counted are exactly the bytes of what is cached, so the bound
\* above is a bound on what the cache really holds. Lean: reachable_safe.
UsedExact ==
  \* The charge is the sum of the cached blocks' sizes.
  used = SumSize(cached)

\* A block that landed for a queue stays readable until that queue's read
\* runs again. Rules out: a CacheOnly read's block lands, the cache evicts
\* it before the owner polls, and the read finds nothing and misses again,
\* forever. Lean: reachable_safe.
LandedReadable ==
  \* Everything that landed for every queue is in memory.
  \A q \in Queues : landed[q] \subseteq live

\* Every block's count is exactly its holders: the cache, the readers, the
\* landings. Lean: reachable_safe (refs_exact).
RefsExact ==
  \* For every block: the count is the cache's one (if cached) ...
  \A b \in Blocks :
    \* ... plus one per holder, plus one per landing.
    refs[b] = (IF b \in cached THEN 1 ELSE 0) + Holders(b) + Landings(b)

====
