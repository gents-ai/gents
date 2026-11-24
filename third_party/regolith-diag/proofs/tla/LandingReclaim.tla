---- MODULE LandingReclaim ----
\* The real thing: a queue reads a block for a waiting call. Two kovan maps
\* name it while the read is fresh: the queue's landing table
\* (QueueShared::landed, src/engine/io/shared.rs) and the database's unit
\* table (IoRuntime::units, src/engine/io/mod.rs), whose unit keeps the block
\* in its outcome. The owner's ledger (Ledger::order, src/io_queue/ledger.rs)
\* decides when the queue lets the landing go. Removing a key from a kovan
\* map only retires its node: the node, and whatever it owns, is freed when
\* kovan reclaims it, and an owner thread that goes idle never takes the step
\* that does. HashMap and HopscotchMap both retire a removed entry so, which
\* is why the fix is the handle the node owns, not the kind of map.
\* A tiny example: a queue reads a 44 KiB block, the call finishes, the next
\* landing pushes the block past the byte bound, and the owner idles. The
\* ledger has let the block go, but a retired map node that owns a strong
\* handle keeps all 44 KiB alive for as long as the thread idles.
\* What must never happen: once the ledger lets a landing go and no reader
\* holds the block, the block stays alive only because a map node owns it.
\* The maps hold weak handles (Posted::Ready and Weak<Unit>), so this holds
\* whatever kovan's reclamation schedule; the RED configurations put a
\* strong handle back in one map each and break it.
\* Correspondence:
\*   ledgerHeld   the landing's entry in Ledger::order (ledger.rs, Ledger::land)
\*   entry[m]     the key's node in map m: posted, retired (removed, not yet
\*                reclaimed) or gone
\*   pins         readers holding the block they upgraded from the table
\*                (IoRuntime::miss_counted through QueueShared::landed)
\*   idle         the owner thread has gone idle and takes no kovan step
\*   Land         IoQueue::land (queue.rs): QueueShared::land then Ledger::land
\*   ForgetUnit   IoRuntime::forget (io/mod.rs), right after the unit ran
\*   Evict        Ledger::settle or Ledger::clear dropping the order entry,
\*                then QueueShared::forget
\*   Reclaim      kovan freeing a retired node, on a later guard or flush
EXTENDS Naturals

\* The two kovan maps that name a landed block.
Maps == {"landed", "units"}
\* The planted bug under test: "none", or the map that keeps a strong handle.
CONSTANT Mutant
\* Only the calibrated mutations are admitted.
ASSUME Mutant \in {"none", "StrongInMap", "StrongUnit"}

\* Whether the ledger's order still holds the landing (its strong handle).
VARIABLE ledgerHeld
\* Whether the ledger has let the landing go.
VARIABLE released
\* Each map's node for the block: "absent", "posted", "retired" or "gone".
VARIABLE entry
\* Readers holding the block now (0 or 1 suffices).
VARIABLE pins
\* Whether the owner thread has gone idle for good.
VARIABLE idle
\* The complete model state.
vars == <<ledgerHeld, released, entry, pins, idle>>

\* Whether map m keeps a strong handle on the block in its node.
Strong(m) == (m = "landed" /\ Mutant = "StrongInMap")
             \* The unit table's node owns the unit, which owns its outcome.
             \/ (m = "units" /\ Mutant = "StrongUnit")
\* Whether map m's node, posted or retired but not yet reclaimed, owns the block.
MapOwns(m) == Strong(m) /\ entry[m] \in {"posted", "retired"}
\* The block is alive while anything holds a strong handle on it.
Alive == ledgerHeld \/ pins > 0 \/ \E m \in Maps : MapOwns(m)

\* Nothing read yet: no node, no landing, the owner awake.
Init == ledgerHeld = FALSE /\ released = FALSE
        \* No map names the block yet.
        /\ entry = [m \in Maps |-> "absent"]
        \* No reader holds it and the owner is polling.
        /\ pins = 0 /\ idle = FALSE

\* The unit finished and the owner landed its block: posted in both maps,
\* held by the ledger.
Land == entry["landed"] = "absent" /\ ~released
        \* The ledger takes its strong handle; both maps post a node.
        /\ ledgerHeld' = TRUE /\ entry' = [m \in Maps |-> "posted"]
        \* Landing changes no reader and does not idle the owner.
        /\ UNCHANGED <<released, pins, idle>>

\* The unit that ran is dropped from the unit table: its node is retired.
ForgetUnit == entry["units"] = "posted"
              \* Only the unit table changes.
              /\ entry' = [entry EXCEPT !["units"] = "retired"]
              \* The ledger, the readers and the owner are untouched.
              /\ UNCHANGED <<ledgerHeld, released, pins, idle>>

\* A reader finds the landing and upgrades its handle: only a live block.
Pin == entry["landed"] = "posted" /\ Alive /\ pins = 0
       \* The reader now holds the block.
       /\ pins' = 1
       \* Reading changes no map, ledger or owner state.
       /\ UNCHANGED <<ledgerHeld, released, entry, idle>>

\* The reader finished with the block and dropped it.
Unpin == pins = 1 /\ pins' = 0
         \* Dropping a pin changes nothing else.
         /\ UNCHANGED <<ledgerHeld, released, entry, idle>>

\* The ledger lets the landing go (settle past the bound, or clear) and
\* forgets the table's key: its node is retired.
Evict == ledgerHeld /\ ledgerHeld' = FALSE /\ released' = TRUE
         \* QueueShared::forget retires the landing table's node.
         /\ entry' = [entry EXCEPT !["landed"] = "retired"]
         \* Readers and the owner are untouched.
         /\ UNCHANGED <<pins, idle>>

\* The owner thread goes idle and stays idle: no later kovan step on it.
GoIdle == ~idle /\ idle' = TRUE
          \* Idling changes no ownership.
          /\ UNCHANGED <<ledgerHeld, released, entry, pins>>

\* kovan reclaims a retired node, which needs a step the idle thread never takes.
Reclaim(m) == ~idle /\ entry[m] = "retired"
              \* The node and what it owns are freed.
              /\ entry' = [entry EXCEPT ![m] = "gone"]
              \* Nothing else changes.
              /\ UNCHANGED <<ledgerHeld, released, pins, idle>>

\* Any of the program's steps may be next, in any order.
Next == Land \/ ForgetUnit \/ Pin \/ Unpin \/ Evict \/ GoIdle
        \* Reclamation of either map's retired node.
        \/ \E m \in Maps : Reclaim(m)

\* Steps happen in any interleaving; nothing forces reclamation.
Spec == Init /\ [][Next]_vars

\* Every state stays within the finite representation.
TypeOK == ledgerHeld \in BOOLEAN /\ released \in BOOLEAN
          \* Each node is in one of its four states.
          /\ entry \in [Maps -> {"absent", "posted", "retired", "gone"}]
          \* At most one reader and a Boolean idle flag.
          /\ pins \in 0..1 /\ idle \in BOOLEAN

\* Once the ledger let the landing go and no reader holds it, the block is
\* freed, whether or not kovan has reclaimed the retired nodes.
INV_ReleasedBlockFreed == (released /\ pins = 0) => ~Alive

\* A reader that holds a block holds a live one: weak handles never hand
\* out a freed block.
PinnedBlockAlive == pins > 0 => Alive

\* Not a promise but a witness: false in the state the bug lives in, a let-go
\* landing whose retired nodes kovan will never reclaim because the owner
\* idles. MC_LandingReclaim_Green_ReachesIdle breaks it, so the GREEN rows
\* check that state.
NeverIdleUnreclaimed == ~(released /\ idle /\ \A m \in Maps : entry[m] = "retired")
====
